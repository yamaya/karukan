//! IME session state machine for macOS.
//!
//! `KarukanSession` wraps `karukan-engine` primitives (`RomajiConverter`,
//! `LearningCache`, `KanaKanjiConverter`) and implements the state machine:
//!
//! ```text
//! Empty ──push_char──→ Composing
//! Composing ──Return──→ Empty         (commit hiragana)
//! Composing ──Escape──→ Empty         (cancel)
//! Composing ──Backspace──→ Composing | Empty
//! Composing ──Space──→ Conversion     (kanji candidate selection)
//! Conversion ──Return──→ Empty        (commit selected candidate)
//! Conversion ──Escape──→ Composing    (back to hiragana editing)
//! Conversion ──Space/Tab/Down──→ Conversion (next candidate)
//! Conversion ──Up──→ Conversion       (previous candidate)
//! ```

use std::ffi::CString;
use std::sync::{Arc, OnceLock};

use karukan_engine::{Backend, BackspaceResult, KanaKanjiConverter, LearningCache, RomajiConverter};

// ---------------------------------------------------------------------------
// Process-wide shared KanaKanjiConverter
// ---------------------------------------------------------------------------

/// Loaded at most once per process; all `KarukanSession` instances share it.
///
/// `OnceLock` provides lock-free reads after the first init. `Arc` lets every
/// session hold a cheap reference without copying the model.
static SHARED_CONVERTER: OnceLock<Arc<KanaKanjiConverter>> = OnceLock::new();

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

/// Candidate list exposed to the FFI layer.
#[derive(Default)]
pub(crate) struct CandidateCache {
    /// Each candidate as a null-terminated CString (valid until next push_*).
    pub items: Vec<CString>,
    /// Currently selected candidate index.
    pub cursor: u32,
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// State held while the user is browsing conversion candidates.
struct ConversionState {
    /// The hiragana reading that was converted (used to restore on Escape).
    hiragana: String,
    /// Ranked candidate list (Learning → Model → Dict).
    candidates: Vec<String>,
    /// Currently highlighted candidate index.
    cursor: usize,
}

enum SessionState {
    /// No pending input.
    Empty,
    /// Accumulating romaji / hiragana input.
    Composing,
    /// Browsing kanji conversion candidates.
    Conversion(ConversionState),
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
    /// Shared kanji converter (loaded once per process, reused by all sessions).
    /// `pub(crate)` to allow `karukan_convert_top1` in ffi/query.rs to clone the Arc.
    pub(crate) converter: Option<Arc<KanaKanjiConverter>>,
    /// Live conversion result (karukan-im の `live.text` に相当).
    ///
    /// Some(_) のとき preedit に変換済みテキストを表示し、Enter で確定する。
    /// push_char のたびに None にリセットされ、バックグラウンド推論完了後に
    /// apply_live_candidate() で再セットされる。
    live_candidate: Option<String>,
    /// Preedit state exposed to FFI.
    pub(crate) preedit: PreeditCache,
    /// Commit state exposed to FFI.
    pub(crate) commit: CommitCache,
    /// Candidate list exposed to FFI.
    pub(crate) candidate_cache: CandidateCache,
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
            converter: None,
            live_candidate: None,
            preedit: PreeditCache::default(),
            commit: CommitCache::default(),
            candidate_cache: CandidateCache::default(),
        }
    }

    /// Load resources from disk (dictionary, learning cache, kanji model).
    ///
    /// Missing files are silently skipped (non-fatal).
    /// Call from a background thread — model download may take time on first run.
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

        // KanaKanjiConverter — shared across all sessions (load once per process).
        //
        // If the shared instance is already available, clone the `Arc` (cheap).
        // Otherwise attempt to load; on success store in the global so the next
        // session skips the expensive work.  On failure leave the global empty so
        // the next session can retry (e.g. after the network becomes available).
        self.converter = if let Some(arc) = SHARED_CONVERTER.get() {
            tracing::info!("KanaKanjiConverter: reusing shared instance");
            Some(Arc::clone(arc))
        } else {
            use karukan_engine::kanji::model_config::registry;
            let load_result = registry()
                .default_variant()
                .ok_or_else(|| "no default variant in models.toml".to_string())
                .and_then(|(family, variant)| {
                    Backend::from_variant(family, variant).map_err(|e| e.to_string())
                })
                .and_then(|backend| {
                    KanaKanjiConverter::new(backend).map_err(|e| e.to_string())
                })
                .map(Arc::new);

            match load_result {
                Ok(arc) => {
                    // Race-safe: if another thread won the set(), use its value.
                    let _ = SHARED_CONVERTER.set(arc);
                    let shared = SHARED_CONVERTER.get().unwrap();
                    tracing::info!("KanaKanjiConverter loaded (shared)");
                    Some(Arc::clone(shared))
                }
                Err(e) => {
                    tracing::warn!("Failed to load KanaKanjiConverter: {}", e);
                    None
                }
            }
        };
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

    /// Composing 状態のひらがなを返す。Composing でなければ `None`。
    ///
    /// バックグラウンドスレッドが推論を起動する前に、メインスレッドで取得するために使う。
    pub fn composing_hiragana(&self) -> Option<&str> {
        if matches!(self.state, SessionState::Composing) && !self.input_buf.text.is_empty() {
            Some(&self.input_buf.text)
        } else {
            None
        }
    }

    /// バックグラウンド推論の結果を適用する（メインスレッドからのみ呼ぶこと）。
    ///
    /// live_candidate をセットして preedit を変換済みテキストに更新する。
    /// Composing 状態でなければ無視する（stale な結果が Conversion 中に届いた場合など）。
    pub fn apply_live_candidate(&mut self, candidate: &str) {
        if !matches!(self.state, SessionState::Composing) {
            return;
        }
        self.live_candidate = Some(candidate.to_string());
        // preedit = 変換済みテキスト + 未確定ローマ字バッファ
        // 例: candidate="日本語", romaji.buffer()="h" → preedit="日本語h"
        // karukan-im の set_composing_state() が live.text + romaji buffer を合成するのと同じ。
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = format!("{}{}", candidate, romaji_buf);
        self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
        // キャレットは preedit 末尾（ローマ字バッファの後ろ）
        self.preedit.caret_bytes = preedit_text.len() as u32;
        self.preedit.dirty = true;
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
        // 新しい文字が入力されたので前回のライブ変換結果を無効化する。
        // 次の triggerLiveConversion が完了したら apply_live_candidate で再セットされる。
        self.live_candidate = None;

        // In Conversion state, any printable char cancels conversion and
        // re-enters Composing (commit the char as new input).
        if matches!(self.state, SessionState::Conversion(_)) {
            self.cancel_conversion();
        }

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
        match key {
            // ── Empty state: pass everything through ──────────────────────
            _ if matches!(self.state, SessionState::Empty) => false,

            // ── Composing state ───────────────────────────────────────────
            KarukanKey::Return if matches!(self.state, SessionState::Composing) => {
                self.do_commit();
                true
            }
            KarukanKey::Escape if matches!(self.state, SessionState::Composing) => {
                self.do_cancel();
                true
            }
            KarukanKey::Backspace if matches!(self.state, SessionState::Composing) => {
                self.do_backspace();
                true
            }
            KarukanKey::Space if matches!(self.state, SessionState::Composing) => {
                self.do_conversion();
                true
            }

            // ── Conversion state ──────────────────────────────────────────
            KarukanKey::Return if matches!(self.state, SessionState::Conversion(_)) => {
                self.commit_current_candidate();
                true
            }
            KarukanKey::Escape if matches!(self.state, SessionState::Conversion(_)) => {
                self.cancel_conversion();
                true
            }
            KarukanKey::Backspace if matches!(self.state, SessionState::Conversion(_)) => {
                self.cancel_conversion(); // Conversion → Composing（ひらがな復元）
                self.do_backspace();      // Composing の末尾1文字削除
                true
            }
            KarukanKey::Space | KarukanKey::Tab | KarukanKey::Down
                if matches!(self.state, SessionState::Conversion(_)) =>
            {
                self.move_candidate(1);
                true
            }
            KarukanKey::Up if matches!(self.state, SessionState::Conversion(_)) => {
                self.move_candidate(-1);
                true
            }

            _ => false,
        }
    }

    /// Select a candidate by index and commit it immediately.
    ///
    /// Used by `candidateSelected(_:)` in Swift (IMKCandidates click).
    /// Returns `true` on success, `false` if not in Conversion state or out of range.
    pub fn select_candidate(&mut self, index: usize) -> bool {
        self.clear_flags();
        let SessionState::Conversion(ref conv) = self.state else {
            return false;
        };
        if index >= conv.candidates.len() {
            return false;
        }
        let selected = conv.candidates[index].clone();
        let hiragana = conv.hiragana.clone();

        if let Some(cache) = &mut self.learning {
            cache.record(&hiragana, &selected);
        }
        self.commit.text = CString::new(selected).unwrap_or_default();
        self.commit.dirty = true;
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.state = SessionState::Empty;
        self.romaji.reset();
        self.input_buf.clear();
        self.update_preedit("");
        true
    }

    // -----------------------------------------------------------------------
    // Private helpers — Composing
    // -----------------------------------------------------------------------

    /// Flush remaining romaji, commit the hiragana (or live conversion result), and reset.
    ///
    /// ライブ変換中（live_candidate が Some）の場合は変換済みテキストをコミットし、
    /// 学習キャッシュに記録する（karukan-im の commit_composing と同じ動作）。
    fn do_commit(&mut self) {
        let prev_len = self.romaji.output().chars().count();
        let _ = self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }

        let committed = if let Some(live) = self.live_candidate.take() {
            // ライブ変換結果をコミット（karukan-im: commit_composing の live.text 分岐）
            let hiragana = self.input_buf.text.clone();
            if let Some(cache) = &mut self.learning {
                cache.record(&hiragana, &live);
            }
            live
        } else {
            std::mem::take(&mut self.input_buf.text)
        };

        self.input_buf.cursor_chars = 0;
        self.romaji.reset();
        self.state = SessionState::Empty;

        self.commit.text = CString::new(committed).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    /// Discard all pending input without committing.
    ///
    /// ライブ変換中（live_candidate が Some）の場合は 2段階動作:
    ///   1回目 Escape: live_candidate をクリアしてひらがな表示に戻る（karukan-im と同じ）
    ///   2回目 Escape: 全キャンセル
    fn do_cancel(&mut self) {
        if self.live_candidate.take().is_some() {
            // 1回目: ひらがな表示に戻るだけ（入力はキャンセルしない）
            let preedit_text = format!("{}{}", self.input_buf.text, self.romaji.buffer());
            self.update_preedit(&preedit_text);
            return;
        }
        // 2回目（または live 変換なし）: 全キャンセル
        self.romaji.reset();
        self.input_buf.clear();
        self.state = SessionState::Empty;
        self.update_preedit("");
    }

    /// Handle a Backspace key press.
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
                // romaji は空だが input_buf に内容があれば直接削除する。
                // cancel_conversion() 後は romaji がリセットされているため
                // RemovedOutput が返らず、ここに落ちる。
                self.input_buf.delete_before_cursor();
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

    // -----------------------------------------------------------------------
    // Private helpers — Conversion
    // -----------------------------------------------------------------------

    /// Trigger kanji conversion from the current hiragana input.
    ///
    /// ライブ変換結果（live_candidate）があれば候補リストの先頭に保存する
    /// （karukan-im の start_conversion における prev_suggest_text と同じ処理）。
    fn do_conversion(&mut self) {
        // Flush pending romaji (e.g. lone "k" → "k" pass-through).
        let prev_len = self.romaji.output().chars().count();
        let _ = self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }

        let hiragana = self.input_buf.text.clone();

        // If buffer is empty, insert a full-width space and stay in Empty.
        if hiragana.is_empty() {
            self.romaji.reset();
            self.live_candidate = None;
            self.commit.text = CString::new("\u{3000}").unwrap_or_default();
            self.commit.dirty = true;
            self.state = SessionState::Empty;
            self.update_preedit("");
            return;
        }

        // ライブ変換結果を取り出す（Space → Conversion 移行前にクリア）
        let prev_live = self.live_candidate.take();

        let mut candidates = self.collect_candidates(&hiragana);

        // ライブ変換結果が候補リストにない場合のみ先頭に挿入する。
        // 推論戦略が変わっても表示していた候補が消えないようにする
        // （karukan-im: start_conversion の prev_suggest_text 処理）。
        if let Some(live) = prev_live {
            if live != hiragana && !candidates.contains(&live) {
                candidates.insert(0, live);
            }
        }

        // Populate candidate cache for FFI.
        self.candidate_cache.items = candidates
            .iter()
            .map(|s| CString::new(s.as_str()).unwrap_or_default())
            .collect();
        self.candidate_cache.cursor = 0;

        // Show first candidate in preedit.
        let first = candidates.first().cloned().unwrap_or_else(|| hiragana.clone());
        self.update_preedit(&first);
        self.preedit.caret_bytes = first.len() as u32;

        self.state = SessionState::Conversion(ConversionState {
            hiragana,
            candidates,
            cursor: 0,
        });
    }

    /// Advance or retreat the selected candidate by `delta` (+1 or -1).
    fn move_candidate(&mut self, delta: i32) {
        let SessionState::Conversion(ref mut conv) = self.state else {
            return;
        };
        let len = conv.candidates.len();
        if len == 0 {
            return;
        }
        conv.cursor = ((conv.cursor as i32 + delta).rem_euclid(len as i32)) as usize;
        self.candidate_cache.cursor = conv.cursor as u32;

        let text = conv.candidates[conv.cursor].clone();
        self.update_preedit(&text);
        self.preedit.caret_bytes = text.len() as u32;
    }

    /// Commit the currently selected candidate.
    fn commit_current_candidate(&mut self) {
        let SessionState::Conversion(ref conv) = self.state else {
            return;
        };
        if conv.candidates.is_empty() {
            return;
        }
        let selected = conv.candidates[conv.cursor].clone();
        let hiragana = conv.hiragana.clone();

        if let Some(cache) = &mut self.learning {
            cache.record(&hiragana, &selected);
        }
        self.commit.text = CString::new(selected).unwrap_or_default();
        self.commit.dirty = true;
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.state = SessionState::Empty;
        self.romaji.reset();
        self.input_buf.clear();
        self.update_preedit("");
    }

    /// Cancel conversion and restore the hiragana preedit.
    fn cancel_conversion(&mut self) {
        let hiragana = match &self.state {
            SessionState::Conversion(conv) => conv.hiragana.clone(),
            _ => return,
        };
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.live_candidate = None;
        self.state = SessionState::Composing;
        // Restore input_buf to the hiragana we were converting from.
        self.input_buf.clear();
        self.input_buf.insert(&hiragana);
        self.romaji.reset();
        self.update_preedit(&hiragana);
    }

    /// Collect conversion candidates: Learning → Model → Dict.
    fn collect_candidates(&self, hiragana: &str) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();

        // 1. Learning cache (highest priority — user's own history).
        if let Some(cache) = &self.learning {
            for (surface, _score) in cache.lookup(hiragana) {
                if !result.contains(&surface) {
                    result.push(surface);
                }
            }
        }

        // 2. Neural model candidates (beam search, up to 9).
        if let Some(conv) = &self.converter {
            match conv.convert(hiragana, "", 9) {
                Ok(model_cands) => {
                    for c in model_cands {
                        if !result.contains(&c) {
                            result.push(c);
                        }
                    }
                }
                Err(e) => tracing::warn!("KanaKanjiConverter::convert failed: {}", e),
            }
        }

        // 3. System dictionary (fallback).
        if let Some(dict) = &self.dict {
            if let Some(lr) = dict.exact_match_search(hiragana) {
                for c in lr.candidates.iter().take(5) {
                    if !result.contains(&c.surface) {
                        result.push(c.surface.clone());
                    }
                }
            }
        }

        // Always include the raw hiragana as last resort.
        if result.is_empty() {
            result.push(hiragana.to_string());
        }
        result
    }

    // -----------------------------------------------------------------------
    // Private helpers — preedit / flags
    // -----------------------------------------------------------------------

    /// Rebuild the cached preedit `CString` and caret position.
    fn update_preedit(&mut self, text: &str) {
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
        s.push_char('k');
        s.push_key(KarukanKey::Backspace);
        assert!(s.is_empty());
        assert_eq!(s.preedit.text.to_str().unwrap(), "");
    }

    #[test]
    fn test_backspace_from_hiragana() {
        let mut s = KarukanSession::new();
        s.push_char('k');
        s.push_char('a');
        s.push_key(KarukanKey::Backspace);
        assert_eq!(s.preedit.text.to_str().unwrap(), "");
        assert!(s.is_empty());
    }

    #[test]
    fn test_backspace_partial() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_char('i');
        s.push_key(KarukanKey::Backspace);
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
        "kka".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.preedit.text.to_str().unwrap(), "っか");
    }

    #[test]
    fn test_youon() {
        let mut s = KarukanSession::new();
        "kya".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.preedit.text.to_str().unwrap(), "きゃ");
    }

    #[test]
    fn test_nn() {
        let mut s = KarukanSession::new();
        "nn".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.preedit.text.to_str().unwrap(), "ん");
    }

    #[test]
    fn test_flush_on_commit() {
        let mut s = KarukanSession::new();
        s.push_char('k');
        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "k");
    }

    #[test]
    fn test_caret_after_hiragana() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        assert_eq!(s.preedit.caret_bytes, 3);
    }

    #[test]
    fn test_caret_with_romaji_buffer() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_char('k');
        assert_eq!(s.preedit.caret_bytes, 4);
    }

    #[test]
    fn test_karukan_key_from_u32() {
        assert_eq!(KarukanKey::from_u32(1), Some(KarukanKey::Return));
        assert_eq!(KarukanKey::from_u32(2), Some(KarukanKey::Backspace));
        assert_eq!(KarukanKey::from_u32(3), Some(KarukanKey::Escape));
        assert_eq!(KarukanKey::from_u32(99), None);
    }

    // ── Conversion state tests (no model — candidates fall back to hiragana) ──

    #[test]
    fn test_space_triggers_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // "あ"
        s.push_key(KarukanKey::Space);
        // Should be in Conversion state; candidate_cache has at least 1 item.
        assert!(!s.is_empty());
        assert!(!s.candidate_cache.items.is_empty());
        assert_eq!(s.candidate_cache.cursor, 0);
    }

    #[test]
    fn test_conversion_escape_restores_composing() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        s.push_key(KarukanKey::Escape);
        // Back to composing; preedit should be "あ" again.
        assert!(!s.is_empty());
        assert_eq!(s.preedit.text.to_str().unwrap(), "あ");
        assert!(s.candidate_cache.items.is_empty());
    }

    #[test]
    fn test_conversion_return_commits() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert!(!s.commit.text.to_str().unwrap().is_empty());
        assert!(s.is_empty());
        assert!(s.candidate_cache.items.is_empty());
    }

    #[test]
    fn test_conversion_next_candidate() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| { s.push_char(c); });
        s.push_key(KarukanKey::Space);
        let count = s.candidate_cache.items.len();
        if count > 1 {
            s.push_key(KarukanKey::Space); // next
            assert_eq!(s.candidate_cache.cursor, 1);
        }
    }

    #[test]
    fn test_select_candidate() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        let ok = s.select_candidate(0);
        assert!(ok);
        assert!(s.commit.dirty);
        assert!(s.is_empty());
    }

    #[test]
    fn test_space_on_empty_inserts_fullwidth_space() {
        let mut s = KarukanSession::new();
        // Space when Empty → pass through (not consumed).
        let consumed = s.push_key(KarukanKey::Space);
        assert!(!consumed);
        assert!(!s.commit.dirty);

        // But composing "a" then space should trigger conversion (not insert 　).
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        // Should be in Conversion, not committing a full-width space.
        assert!(!s.commit.dirty);
    }
}
