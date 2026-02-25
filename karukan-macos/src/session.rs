//! IME session state machine for macOS.
//!
//! `KarukanSession` wraps `karukan-engine` primitives (`RomajiConverter`,
//! `LearningCache`) and implements the Phase 1 state machine:
//!
//! ```text
//! Empty ──push_char──→ Composing
//! Composing ──Return──→ Empty  (commit generated)
//! Composing ──Escape──→ Empty  (cancelled)
//! Composing ──Backspace──→ Composing | Empty
//! ```
//!
//! Phase 3 will add a `Conversion` state for kanji candidate selection.

use std::ffi::CString;

use karukan_engine::{BackspaceResult, LearningCache, RomajiConverter};

// ---------------------------------------------------------------------------
// InputBuffer
// ---------------------------------------------------------------------------

/// Hiragana input buffer with cursor tracking.
///
/// The `text` field holds the committed hiragana string. `cursor_chars` is a
/// character (code point) offset into `text` indicating where new characters
/// are inserted and backspace deletes from.
struct InputBuffer {
    /// Composed hiragana text (UTF-8).
    text: String,
    /// Cursor position as a character offset into `text`.
    cursor_chars: usize,
}

impl InputBuffer {
    fn new() -> Self {
        Self {
            text: String::new(),
            cursor_chars: 0,
        }
    }

    #[allow(dead_code)] // may be used in Phase 3
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor_chars = 0;
    }

    /// Insert `s` at the current cursor position, then advance the cursor.
    fn insert(&mut self, s: &str) {
        let byte_pos = self
            .text
            .char_indices()
            .nth(self.cursor_chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len());
        self.text.insert_str(byte_pos, s);
        self.cursor_chars += s.chars().count();
    }

    /// Delete the character immediately before the cursor.
    /// Returns the deleted character, or `None` if the cursor is at position 0.
    fn delete_before_cursor(&mut self) -> Option<char> {
        if self.cursor_chars == 0 {
            return None;
        }
        let char_pos = self.cursor_chars - 1;
        let byte_pos = self.text.char_indices().nth(char_pos).map(|(i, _)| i)?;
        let ch = self.text.remove(byte_pos);
        self.cursor_chars -= 1;
        Some(ch)
    }

    /// Byte offset of the cursor position in `text` (for FFI preedit caret).
    fn cursor_byte_offset(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor_chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

// ---------------------------------------------------------------------------
// KarukanKey
// ---------------------------------------------------------------------------

/// Special keys that the IME recognises (macOS-native; no X11 keysyms).
///
/// The numeric values are the same constants exposed in `karukan_macos.h`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KarukanKey {
    Return = 1,
    Backspace = 2,
    Escape = 3,
    Space = 4,
    Left = 5,
    Right = 6,
    Up = 7,
    Down = 8,
    Tab = 9,
}

impl KarukanKey {
    /// Convert a raw `u32` from the C API to a `KarukanKey`, or `None` if unknown.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Self::Return),
            2 => Some(Self::Backspace),
            3 => Some(Self::Escape),
            4 => Some(Self::Space),
            5 => Some(Self::Left),
            6 => Some(Self::Right),
            7 => Some(Self::Up),
            8 => Some(Self::Down),
            9 => Some(Self::Tab),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Caches shared with FFI layer
// ---------------------------------------------------------------------------

/// Cached preedit string returned through the FFI.
#[derive(Default)]
pub(crate) struct PreeditCache {
    /// Null-terminated UTF-8 preedit text.
    pub text: CString,
    /// Caret byte offset within `text`.
    pub caret_bytes: u32,
    /// `true` if the preedit has changed since the last `clear_flags()`.
    pub dirty: bool,
}

/// Cached commit string returned through the FFI.
#[derive(Default)]
pub(crate) struct CommitCache {
    /// Null-terminated UTF-8 committed text.
    pub text: CString,
    /// `true` if there is a pending commit.
    pub dirty: bool,
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

enum SessionState {
    /// No pending input.
    Empty,
    /// Accumulating romaji / hiragana input.
    Composing,
    // `Conversion` (candidate selection) will be added in Phase 3.
}

// ---------------------------------------------------------------------------
// KarukanSession
// ---------------------------------------------------------------------------

/// The IME session for a single input context.
///
/// Owned by Swift via a raw pointer (`*mut KarukanSession`). All public
/// methods are called through the C FFI layer in `ffi/`.
pub struct KarukanSession {
    state: SessionState,
    /// Romaji-to-hiragana converter.  Its `output()` is cumulative; we track
    /// the delta to build `input_buf` incrementally.
    romaji: RomajiConverter,
    /// Hiragana text accumulated so far (source of truth for preedit).
    input_buf: InputBuffer,
    /// Optional system dictionary (loaded by `init_resources`).
    dict: Option<karukan_engine::Dictionary>,
    /// Optional learning cache (loaded by `init_resources`).
    learning: Option<LearningCache>,
    /// Preedit state exposed to FFI.
    pub(crate) preedit: PreeditCache,
    /// Commit state exposed to FFI.
    pub(crate) commit: CommitCache,
}

impl KarukanSession {
    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Create a new, uninitialised session (fast; no I/O).
    pub fn new() -> Self {
        Self {
            state: SessionState::Empty,
            romaji: RomajiConverter::new(),
            input_buf: InputBuffer::new(),
            dict: None,
            learning: None,
            preedit: PreeditCache::default(),
            commit: CommitCache::default(),
        }
    }

    /// Load resources from disk (dictionary, learning cache).
    ///
    /// Missing files are silently skipped (non-fatal).
    /// Phase 3 will add model loading here; call from a background thread.
    pub fn init_resources(&mut self) {
        use crate::platform::paths;

        // System dictionary (optional — may not exist yet)
        let dict_path = paths::system_dict_path();
        if dict_path.exists() {
            match karukan_engine::Dictionary::load(&dict_path) {
                Ok(d) => {
                    tracing::info!("Loaded system dictionary from {:?}", dict_path);
                    self.dict = Some(d);
                }
                Err(e) => tracing::warn!("Failed to load system dictionary: {}", e),
            }
        }

        // Learning cache (create empty if file doesn't exist yet)
        let learning_path = paths::learning_cache_path();
        self.learning = Some(if learning_path.exists() {
            match LearningCache::load(&learning_path, LearningCache::DEFAULT_MAX_ENTRIES) {
                Ok(c) => {
                    tracing::info!("Loaded learning cache from {:?}", learning_path);
                    c
                }
                Err(e) => {
                    tracing::warn!("Failed to load learning cache, starting empty: {}", e);
                    LearningCache::new(LearningCache::DEFAULT_MAX_ENTRIES)
                }
            }
        } else {
            LearningCache::new(LearningCache::DEFAULT_MAX_ENTRIES)
        });

        // Phase 3: KanaKanjiConverter model loading will be added here.
    }

    /// Persist the learning cache to disk if it has unsaved changes.
    pub fn save_learning(&mut self) {
        let Some(cache) = &mut self.learning else {
            return;
        };
        if !cache.is_dirty() {
            return;
        }

        let path = crate::platform::paths::learning_cache_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match cache.save(&path) {
            Ok(()) => tracing::debug!("Learning cache saved to {:?}", path),
            Err(e) => tracing::warn!("Failed to save learning cache: {}", e),
        }
    }

    // -----------------------------------------------------------------------
    // State queries
    // -----------------------------------------------------------------------

    /// Returns `true` when the session has no pending input.
    pub fn is_empty(&self) -> bool {
        matches!(self.state, SessionState::Empty)
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    /// Push a printable character into the IME.
    ///
    /// Returns `true` if the IME consumed the character (always `true` for
    /// printable input while composing or when starting composition).
    pub fn push_char(&mut self, ch: char) -> bool {
        self.clear_flags();

        // Track the previous output length so we can compute the delta.
        let prev_output_chars = self.romaji.output().chars().count();

        // Feed the character to the romaji converter.
        let _event = self.romaji.push(ch);

        // Append only the newly produced hiragana to input_buf.
        let new_hiragana: String = self
            .romaji
            .output()
            .chars()
            .skip(prev_output_chars)
            .collect();
        if !new_hiragana.is_empty() {
            self.input_buf.insert(&new_hiragana);
        }

        // Build preedit = confirmed hiragana + pending romaji buffer.
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = format!("{}{}", self.input_buf.text, romaji_buf);

        if preedit_text.is_empty() {
            self.state = SessionState::Empty;
            self.update_preedit("");
        } else {
            self.state = SessionState::Composing;
            self.update_preedit(&preedit_text);
        }

        true // always consumed
    }

    /// Push a special key into the IME.
    ///
    /// Returns `true` if the IME consumed the key, `false` if the key should
    /// be passed through to the application (e.g. when the session is empty).
    pub fn push_key(&mut self, key: KarukanKey) -> bool {
        self.clear_flags();
        match (&self.state, key) {
            // Nothing pending — let the application handle the key.
            (SessionState::Empty, _) => false,

            (SessionState::Composing, KarukanKey::Return) => {
                self.do_commit();
                true
            }
            (SessionState::Composing, KarukanKey::Escape) => {
                self.do_cancel();
                true
            }
            (SessionState::Composing, KarukanKey::Backspace) => {
                self.do_backspace();
                true
            }
            // Phase 1: Space commits a full-width space as part of composition.
            // Phase 3 will change this to trigger kanji candidate conversion.
            (SessionState::Composing, KarukanKey::Space) => {
                self.input_buf.insert("\u{3000}"); // 全角スペース U+3000
                let preedit = format!("{}{}", self.input_buf.text, self.romaji.buffer());
                self.update_preedit(&preedit);
                true
            }
            // Left/Right/Up/Down/Tab: pass through in Phase 1.
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Flush remaining romaji, commit the hiragana, and reset.
    fn do_commit(&mut self) {
        // Flush pending romaji (e.g. lone "k" → "k" pass-through).
        let prev_len = self.romaji.output().chars().count();
        let _ = self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }

        let committed = std::mem::take(&mut self.input_buf.text);
        self.input_buf.cursor_chars = 0;
        self.romaji.reset();
        self.state = SessionState::Empty;

        self.commit.text = CString::new(committed).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    /// Discard all pending input without committing.
    fn do_cancel(&mut self) {
        self.romaji.reset();
        self.input_buf.clear();
        self.state = SessionState::Empty;
        self.update_preedit("");
    }

    /// Handle a Backspace key press.
    ///
    /// The romaji converter owns two layers of input:
    /// 1. `buffer` — pending romaji not yet converted (e.g. "k", "sh")
    /// 2. `output` — already converted hiragana (mirrored in `input_buf`)
    ///
    /// `RomajiConverter::backspace()` removes one character from whichever
    /// layer is non-empty (buffer first, then output). When it removes from
    /// `output` we must also remove the corresponding character from `input_buf`
    /// because `push_char()` keeps them in sync via delta tracking.
    fn do_backspace(&mut self) {
        match self.romaji.backspace() {
            BackspaceResult::RemovedBuffer(_) => {
                // Removed from pending romaji buffer only; input_buf unchanged.
            }
            BackspaceResult::RemovedOutput(_) => {
                // Removed from confirmed output; input_buf must be synced.
                self.input_buf.delete_before_cursor();
            }
            BackspaceResult::Empty => {
                // Nothing to delete — should not normally reach here while Composing.
            }
        }

        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = format!("{}{}", self.input_buf.text, romaji_buf);

        if preedit_text.is_empty() {
            self.state = SessionState::Empty;
            self.update_preedit("");
        } else {
            self.update_preedit(&preedit_text);
        }
    }

    /// Rebuild the cached preedit `CString` and caret position.
    fn update_preedit(&mut self, text: &str) {
        // Caret sits at the end of confirmed hiragana + romaji buffer.
        let caret_bytes = self.input_buf.cursor_byte_offset() + self.romaji.buffer().len();
        self.preedit.text = CString::new(text).unwrap_or_default();
        self.preedit.caret_bytes = caret_bytes as u32;
        self.preedit.dirty = true;
    }

    /// Clear the dirty flags before processing a new input event.
    fn clear_flags(&mut self) {
        self.preedit.dirty = false;
        self.commit.dirty = false;
    }
}

impl Default for KarukanSession {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Unit tests (pure Rust, no FFI)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_char_a() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        assert_eq!(s.preedit.text.to_str().unwrap(), "あ");
        assert!(s.preedit.dirty);
    }

    #[test]
    fn test_push_char_ka() {
        let mut s = KarukanSession::new();
        s.push_char('k');
        assert_eq!(s.preedit.text.to_str().unwrap(), "k");
        s.push_char('a');
        assert_eq!(s.preedit.text.to_str().unwrap(), "か");
    }

    #[test]
    fn test_commit_on_return() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_char('i');
        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "あい");
        assert!(s.is_empty());
    }

    #[test]
    fn test_escape_cancel() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_char('i');
        s.push_key(KarukanKey::Escape);
        assert!(!s.commit.dirty);
        assert!(s.is_empty());
        assert_eq!(s.preedit.text.to_str().unwrap(), "");
    }

    #[test]
    fn test_backspace_from_romaji_buffer() {
        let mut s = KarukanSession::new();
        s.push_char('k'); // "k" in romaji buffer, preedit = "k"
        s.push_key(KarukanKey::Backspace);
        // Buffer cleared → empty
        assert!(s.is_empty());
        assert_eq!(s.preedit.text.to_str().unwrap(), "");
    }

    #[test]
    fn test_backspace_from_hiragana() {
        let mut s = KarukanSession::new();
        s.push_char('k');
        s.push_char('a'); // → "か"
        s.push_key(KarukanKey::Backspace); // removes "か" from output
        assert_eq!(s.preedit.text.to_str().unwrap(), "");
        assert!(s.is_empty());
    }

    #[test]
    fn test_backspace_partial() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // あ
        s.push_char('i'); // い
        s.push_key(KarukanKey::Backspace); // removes い
        assert_eq!(s.preedit.text.to_str().unwrap(), "あ");
        assert!(!s.is_empty());
    }

    #[test]
    fn test_empty_state_keys_pass_through() {
        let mut s = KarukanSession::new();
        assert!(s.is_empty());
        assert!(!s.push_key(KarukanKey::Return));
        assert!(!s.push_key(KarukanKey::Backspace));
        assert!(!s.push_key(KarukanKey::Escape));
    }

    #[test]
    fn test_sokuon() {
        let mut s = KarukanSession::new();
        "kka".chars().for_each(|c| {
            s.push_char(c);
        });
        assert_eq!(s.preedit.text.to_str().unwrap(), "っか");
    }

    #[test]
    fn test_youon() {
        let mut s = KarukanSession::new();
        "kya".chars().for_each(|c| {
            s.push_char(c);
        });
        assert_eq!(s.preedit.text.to_str().unwrap(), "きゃ");
    }

    #[test]
    fn test_nn() {
        let mut s = KarukanSession::new();
        "nn".chars().for_each(|c| {
            s.push_char(c);
        });
        assert_eq!(s.preedit.text.to_str().unwrap(), "ん");
    }

    #[test]
    fn test_flush_on_commit() {
        let mut s = KarukanSession::new();
        s.push_char('k'); // pending romaji "k"
        s.push_key(KarukanKey::Return); // flush → commit "k"
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "k");
    }

    #[test]
    fn test_caret_after_hiragana() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // "あ" (3 bytes)
        // caret should be at end = 3 bytes
        assert_eq!(s.preedit.caret_bytes, 3);
    }

    #[test]
    fn test_caret_with_romaji_buffer() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // "あ" confirmed
        s.push_char('k'); // "k" in romaji buffer
        // preedit = "あk", caret = 3 (あ) + 1 (k) = 4 bytes
        assert_eq!(s.preedit.caret_bytes, 4);
    }

    #[test]
    fn test_karukan_key_from_u32() {
        assert_eq!(KarukanKey::from_u32(1), Some(KarukanKey::Return));
        assert_eq!(KarukanKey::from_u32(2), Some(KarukanKey::Backspace));
        assert_eq!(KarukanKey::from_u32(3), Some(KarukanKey::Escape));
        assert_eq!(KarukanKey::from_u32(99), None);
    }
}
