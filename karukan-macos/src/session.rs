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
    ConvertHiragana = 10,
    ConvertKatakana = 11,
    ConvertAscii = 12,
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
            10 => Some(Self::ConvertHiragana),
            11 => Some(Self::ConvertKatakana),
            12 => Some(Self::ConvertAscii),
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
    /// apply_live_candidate() でセットされ、do_commit/do_cancel/do_backspace 等でクリアされる。
    live_candidate: Option<String>,
    /// live_candidate を生成したときの input_buf.text（= 推論時の composing hiragana）。
    /// do_commit 時に input_buf.text と照合し、一致しない場合は live_candidate を stale として無視する。
    /// これにより "なでし" 推論結果が適用された後に 'o' を追加して "なでしこ" になっても、
    /// Return で "なでし" がコミットされるバグを防ぐ。
    live_candidate_source: String,
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
            live_candidate_source: String::new(),
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

    /// romaji converter に未確定の子音が残っているか。
    /// 「k」「sh」「ch」など母音待ちの状態で `true` を返す。
    /// Swift 側が preedit 遅延の判定に使う。
    pub fn is_consonant_pending(&self) -> bool {
        !self.romaji.buffer().is_empty()
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
    ///
    /// `source` は推論を開始した時点の `input_buf.text`（= karukan_get_composing_hiragana の戻り値）。
    /// 現在の `input_buf.text` と一致しない場合は stale な結果として無視する。
    /// これにより、'k' 押下時（composing="なでし"）に開始した推論が 'o' 入力後（composing="なでしこ"）
    /// に完了しても誤って "なでし" 変換結果が適用されるバグを防ぐ。
    ///
    /// 新しいライブ変換サイクルの開始を意味するため、clear_flags() で前サイクルの
    /// dirty フラグ（特に commit.dirty）をクリアする。これにより、文節分割コミット後の
    /// 残余ひらがなの推論完了時に前のコミットテキストが重複送信されるバグを防ぐ。
    pub fn apply_live_candidate(&mut self, candidate: &str, source: &str) {
        if !matches!(self.state, SessionState::Composing) {
            return;
        }
        // source が現在の input_buf.text と異なる場合は stale — 無視する。
        if source != self.input_buf.text {
            return;
        }
        self.clear_flags();
        self.live_candidate = Some(candidate.to_string());
        self.live_candidate_source = source.to_string();
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

    /// 長文コミット後の残余ひらがなを Composing 状態として注入する。
    ///
    /// `push_key(Return)` でのコミット完了直後にメインスレッドから呼び、
    /// 文節分割の後半を次の Composing 入力として引き継ぐ。
    ///
    /// - romaji バッファをリセット（前の入力残留を防ぐ）
    /// - `input_buf` にひらがなをセット
    /// - `live_candidate` をクリア
    /// - `SessionState::Composing` へ遷移
    /// - preedit をひらがな表示に更新
    pub fn set_composing_hiragana(&mut self, hiragana: &str) {
        if hiragana.is_empty() {
            return;
        }
        self.romaji.reset();
        self.input_buf.clear();
        self.input_buf.insert(hiragana);
        self.live_candidate = None;
        self.state = SessionState::Composing;
        // preedit をひらがなで更新（カーソルは末尾）
        self.preedit.text = CString::new(hiragana).unwrap_or_default();
        self.preedit.caret_bytes = hiragana.len() as u32;
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
        // ライブ変換結果が残っていれば、推論完了まで表示ベースとして使い続ける。
        // clone() で参照し live_candidate は保持する。apply_live_candidate() が
        // 新しい結果で上書きするか、do_commit/do_cancel で消費される。
        // take() にすると連続キー入力で None になりひらがなフォールバックが起きる。
        let prev_live = self.live_candidate.clone();

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

        // Build preedit:
        // prev_live がある場合（例: "今日"）はそれをベースにする。
        //   - 新しいかなが生成された場合: prev_live + new_hiragana + romaji_buf
        //     例: "今日" + "は" + "" = "今日は"（"今日h" → "今日は" に直接遷移）
        //   - 子音のみ追加された場合: prev_live + romaji_buf
        //     例: "今日" + "h" = "今日h"
        // これにより文字数が一時的に減る中間状態（"今日" のみ）を避け、
        // カーソルの前後ジャンプを防ぐ。推論完了後は apply_live_candidate が上書きする。
        // prev_live がなければ従来通り input_buf ひらがな + ローマ字バッファ。
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = if let Some(ref live) = prev_live {
            format!("{}{}{}", live, new_hiragana, romaji_buf)
        } else {
            format!("{}{}", self.input_buf.text, romaji_buf)
        };

        if preedit_text.is_empty() {
            self.state = SessionState::Empty;
            self.update_preedit("");
        } else {
            self.state = SessionState::Composing;
            if prev_live.is_some() {
                // ライブ変換結果ベースの preedit を直接セット。
                // update_preedit() は input_buf ベースのカーソルを使うため使えない。
                self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
                self.preedit.caret_bytes = preedit_text.len() as u32;
                self.preedit.dirty = true;
            } else {
                self.update_preedit(&preedit_text);
            }
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

            // ── Convert shortcuts (Composing + Conversion) ───────────────
            KarukanKey::ConvertHiragana => {
                self.do_convert_hiragana();
                true
            }
            KarukanKey::ConvertKatakana => {
                self.do_convert_katakana();
                true
            }
            KarukanKey::ConvertAscii => {
                self.do_convert_ascii();
                true
            }

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
            // live_candidate_source が現在の input_buf.text と一致する場合のみ採用。
            // 'k' 押下時（composing="なでし"）に生成された live_candidate が
            // 'o' 入力後（composing="なでしこ"）にコミットされるバグを防ぐ。
            if self.live_candidate_source == self.input_buf.text {
                let hiragana = self.input_buf.text.clone();
                if let Some(cache) = &mut self.learning {
                    cache.record(&hiragana, &live);
                }
                live
            } else {
                std::mem::take(&mut self.input_buf.text)
            }
        } else {
            std::mem::take(&mut self.input_buf.text)
        };

        // live/non-live どちらの経路でも input_buf を完全にクリアする。
        // non-live は std::mem::take で text は既に空だが cursor_chars のリセットを兼ねる。
        // live は take() せず text が残ったままなので clear() が必須（次のキー入力で
        // 古いひらがなが preedit に混入するバグを防ぐ）。
        self.input_buf.clear();
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

        // Backspace で文字が減ったのでライブ変換結果は stale — クリアする。
        // 次の triggerLiveConversion で新しい結果が apply される。
        self.live_candidate = None;

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
    // Private helpers — Convert shortcuts (Ctrl+J / Ctrl+K / Ctrl+;)
    // -----------------------------------------------------------------------

    /// Flush pending romaji buffer into `input_buf`.
    fn flush_romaji(&mut self) {
        let prev_len = self.romaji.output().chars().count();
        let _ = self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }
    }

    /// Restore hiragana from Conversion state before converting.
    ///
    /// If in Conversion state, restores `input_buf` from the saved hiragana
    /// and transitions to Composing so that `do_convert_*` can operate on it.
    fn restore_hiragana_if_conversion(&mut self) {
        if let SessionState::Conversion(ref conv) = self.state {
            let hiragana = conv.hiragana.clone();
            self.candidate_cache.items.clear();
            self.candidate_cache.cursor = 0;
            self.input_buf.clear();
            self.input_buf.insert(&hiragana);
            self.romaji.reset();
            self.state = SessionState::Composing;
        }
    }

    /// Ctrl+J: ひらがなのまま確定。
    fn do_convert_hiragana(&mut self) {
        self.restore_hiragana_if_conversion();
        self.flush_romaji();
        if self.input_buf.text.is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.input_buf.text);
        self.live_candidate = None;
        self.input_buf.cursor_chars = 0;
        self.romaji.reset();
        self.state = SessionState::Empty;
        self.commit.text = CString::new(text).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    /// Ctrl+K: カタカナに変換して確定。
    fn do_convert_katakana(&mut self) {
        self.restore_hiragana_if_conversion();
        self.flush_romaji();
        if self.input_buf.text.is_empty() {
            return;
        }

        let katakana = karukan_engine::kana::hiragana_to_katakana(&self.input_buf.text);
        self.live_candidate = None;
        self.input_buf.clear();
        self.romaji.reset();
        self.state = SessionState::Empty;
        self.commit.text = CString::new(katakana).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    /// Ctrl+;: 半角英数（ローマ字）に逆変換して確定。
    fn do_convert_ascii(&mut self) {
        self.restore_hiragana_if_conversion();
        self.flush_romaji();
        if self.input_buf.text.is_empty() {
            return;
        }

        let romaji = hiragana_to_romaji(&self.input_buf.text);
        self.live_candidate = None;
        self.input_buf.clear();
        self.romaji.reset();
        self.state = SessionState::Empty;
        self.commit.text = CString::new(romaji).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
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
// Reverse romaji table (hiragana → romaji)
// ---------------------------------------------------------------------------

use std::sync::LazyLock;

/// ひらがな→ローマ字の逆引きテーブル。
///
/// `karukan-engine` の `rules.rs` と同一のマッピングを逆方向にしたもの。
/// 複数のローマ字表記がある場合は最も一般的なものを採用（例: "し" → "shi"）。
/// エントリはひらがなの長い順にソート済み（最長一致のため）。
static REVERSE_ROMAJI: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| {
    let mut table: Vec<(&str, &str)> = vec![
        // ── 拗音・特殊音（2文字以上のかな） ──
        // きゃ行
        ("きゃ", "kya"), ("きゅ", "kyu"), ("きょ", "kyo"),
        ("きぃ", "kyi"), ("きぇ", "kye"),
        // くぁ行
        ("くぁ", "kwa"), ("くぃ", "kwi"), ("くぅ", "kwu"),
        ("くぇ", "kwe"), ("くぉ", "kwo"),
        // ぎゃ行
        ("ぎゃ", "gya"), ("ぎゅ", "gyu"), ("ぎょ", "gyo"),
        ("ぎぃ", "gyi"), ("ぎぇ", "gye"),
        // ぐぁ行
        ("ぐぁ", "gwa"), ("ぐぃ", "gwi"), ("ぐぅ", "gwu"),
        ("ぐぇ", "gwe"), ("ぐぉ", "gwo"),
        // しゃ行
        ("しゃ", "sha"), ("しゅ", "shu"), ("しょ", "sho"),
        ("しぃ", "syi"), ("しぇ", "she"),
        // すぁ行
        ("すぁ", "swa"), ("すぃ", "swi"), ("すぅ", "swu"),
        ("すぇ", "swe"), ("すぉ", "swo"),
        // じゃ行
        ("じゃ", "ja"), ("じゅ", "ju"), ("じょ", "jo"),
        ("じぃ", "zyi"), ("じぇ", "je"),
        // ずぁ行
        ("ずぁ", "zwa"), ("ずぃ", "zwi"), ("ずぅ", "zwu"),
        ("ずぇ", "zwe"), ("ずぉ", "zwo"),
        // ちゃ行
        ("ちゃ", "cha"), ("ちゅ", "chu"), ("ちょ", "cho"),
        ("ちぃ", "tyi"), ("ちぇ", "che"),
        // つぁ行
        ("つぁ", "tsa"), ("つぃ", "tsi"), ("つぇ", "tse"), ("つぉ", "tso"),
        // てゃ行
        ("てゃ", "tha"), ("てぃ", "thi"), ("てゅ", "thu"),
        ("てぇ", "the"), ("てょ", "tho"),
        // とぁ行
        ("とぁ", "twa"), ("とぃ", "twi"), ("とぅ", "twu"),
        ("とぇ", "twe"), ("とぉ", "two"),
        // ぢゃ行
        ("ぢゃ", "dya"), ("ぢゅ", "dyu"), ("ぢょ", "dyo"),
        ("ぢぃ", "dyi"), ("ぢぇ", "dye"),
        // でゃ行
        ("でゃ", "dha"), ("でぃ", "dhi"), ("でゅ", "dhu"),
        ("でぇ", "dhe"), ("でょ", "dho"),
        // どぁ行
        ("どぁ", "dwa"), ("どぃ", "dwi"), ("どぅ", "dwu"),
        ("どぇ", "dwe"), ("どぉ", "dwo"),
        // にゃ行
        ("にゃ", "nya"), ("にゅ", "nyu"), ("にょ", "nyo"),
        ("にぃ", "nyi"), ("にぇ", "nye"),
        // ひゃ行
        ("ひゃ", "hya"), ("ひゅ", "hyu"), ("ひょ", "hyo"),
        ("ひぃ", "hyi"), ("ひぇ", "hye"),
        // ふぁ行
        ("ふぁ", "fa"), ("ふぃ", "fi"), ("ふぇ", "fe"), ("ふぉ", "fo"),
        ("ふゃ", "fya"), ("ふゅ", "fyu"), ("ふょ", "fyo"),
        // びゃ行
        ("びゃ", "bya"), ("びゅ", "byu"), ("びょ", "byo"),
        ("びぃ", "byi"), ("びぇ", "bye"),
        // ぴゃ行
        ("ぴゃ", "pya"), ("ぴゅ", "pyu"), ("ぴょ", "pyo"),
        ("ぴぃ", "pyi"), ("ぴぇ", "pye"),
        // みゃ行
        ("みゃ", "mya"), ("みゅ", "myu"), ("みょ", "myo"),
        ("みぃ", "myi"), ("みぇ", "mye"),
        // りゃ行
        ("りゃ", "rya"), ("りゅ", "ryu"), ("りょ", "ryo"),
        ("りぃ", "ryi"), ("りぇ", "rye"),
        // うぁ行
        ("うぁ", "wha"), ("うぃ", "wi"), ("うぇ", "we"), ("うぉ", "who"),
        // いぇ
        ("いぇ", "ye"),
        // ゔ行
        ("ゔぁ", "va"), ("ゔぃ", "vi"), ("ゔぇ", "ve"), ("ゔぉ", "vo"),
        ("ゔゃ", "vya"), ("ゔゅ", "vyu"), ("ゔょ", "vyo"),

        // ── 単独かな ──
        ("あ", "a"), ("い", "i"), ("う", "u"), ("え", "e"), ("お", "o"),
        ("か", "ka"), ("き", "ki"), ("く", "ku"), ("け", "ke"), ("こ", "ko"),
        ("さ", "sa"), ("し", "shi"), ("す", "su"), ("せ", "se"), ("そ", "so"),
        ("た", "ta"), ("ち", "chi"), ("つ", "tsu"), ("て", "te"), ("と", "to"),
        ("な", "na"), ("に", "ni"), ("ぬ", "nu"), ("ね", "ne"), ("の", "no"),
        ("は", "ha"), ("ひ", "hi"), ("ふ", "fu"), ("へ", "he"), ("ほ", "ho"),
        ("ま", "ma"), ("み", "mi"), ("む", "mu"), ("め", "me"), ("も", "mo"),
        ("や", "ya"), ("ゆ", "yu"), ("よ", "yo"),
        ("ら", "ra"), ("り", "ri"), ("る", "ru"), ("れ", "re"), ("ろ", "ro"),
        ("わ", "wa"), ("を", "wo"), ("ん", "nn"),
        // 濁音
        ("が", "ga"), ("ぎ", "gi"), ("ぐ", "gu"), ("げ", "ge"), ("ご", "go"),
        ("ざ", "za"), ("じ", "ji"), ("ず", "zu"), ("ぜ", "ze"), ("ぞ", "zo"),
        ("だ", "da"), ("ぢ", "di"), ("づ", "du"), ("で", "de"), ("ど", "do"),
        ("ば", "ba"), ("び", "bi"), ("ぶ", "bu"), ("べ", "be"), ("ぼ", "bo"),
        // 半濁音
        ("ぱ", "pa"), ("ぴ", "pi"), ("ぷ", "pu"), ("ぺ", "pe"), ("ぽ", "po"),
        // ゔ
        ("ゔ", "vu"),
        // 小文字
        ("ぁ", "xa"), ("ぃ", "xi"), ("ぅ", "xu"), ("ぇ", "xe"), ("ぉ", "xo"),
        ("ゃ", "xya"), ("ゅ", "xyu"), ("ょ", "xyo"),
        ("っ", "xtu"), ("ゎ", "xwa"),
        // 歴史的かな
        ("ゐ", "wyi"), ("ゑ", "wye"),
        // 長音記号
        ("ー", "-"),
        // 句読点・記号
        ("、", ","), ("。", "."), ("・", "/"),
        ("？", "?"), ("！", "!"), ("〜", "~"),
        ("「", "["), ("」", "]"),
        ("『", "z["), ("』", "z]"),
        ("…", "z."), ("‥", "z,"),
        ("←", "zh"), ("↓", "zj"), ("↑", "zk"), ("→", "zl"),
    ];
    // ひらがなの長い順にソート（最長一致）
    table.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    table
});

/// ひらがなを最長一致でローマ字に逆変換する。
fn hiragana_to_romaji(hiragana: &str) -> String {
    let mut result = String::new();
    let mut pos = 0;
    let bytes = hiragana.as_bytes();
    while pos < bytes.len() {
        let remaining = &hiragana[pos..];
        let mut matched = false;
        for &(kana, romaji) in REVERSE_ROMAJI.iter() {
            if remaining.starts_with(kana) {
                result.push_str(romaji);
                pos += kana.len();
                matched = true;
                break;
            }
        }
        if !matched {
            // テーブルにない文字はそのまま出力
            let ch = remaining.chars().next().unwrap();
            result.push(ch);
            pos += ch.len_utf8();
        }
    }
    result
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

    // ── Convert shortcut tests ──

    #[test]
    fn test_convert_hiragana_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.preedit.text.to_str().unwrap(), "にほんご");
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "にほんご");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_katakana_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| { s.push_char(c); });
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "ニホンゴ");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_ascii_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| { s.push_char(c); });
        s.push_key(KarukanKey::ConvertAscii);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "nihonngo");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_hiragana_from_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space); // enter Conversion
        assert!(!s.candidate_cache.items.is_empty());
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "あ");
        assert!(s.is_empty());
        assert!(s.candidate_cache.items.is_empty());
    }

    #[test]
    fn test_convert_katakana_from_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "ア");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_ascii_from_conversion() {
        let mut s = KarukanSession::new();
        "ka".chars().for_each(|c| { s.push_char(c); });
        s.push_key(KarukanKey::Space);
        s.push_key(KarukanKey::ConvertAscii);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "ka");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_on_empty_not_consumed() {
        let mut s = KarukanSession::new();
        assert!(!s.push_key(KarukanKey::ConvertHiragana));
        assert!(!s.push_key(KarukanKey::ConvertKatakana));
        assert!(!s.push_key(KarukanKey::ConvertAscii));
    }

    #[test]
    fn test_convert_flushes_pending_romaji() {
        let mut s = KarukanSession::new();
        s.push_char('k'); // pending romaji "k"
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        // "k" should be flushed and committed
        assert_eq!(s.commit.text.to_str().unwrap(), "k");
        assert!(s.is_empty());
    }

    // ── Reverse romaji table tests ──

    #[test]
    fn test_reverse_romaji_basic() {
        assert_eq!(hiragana_to_romaji("あいうえお"), "aiueo");
        assert_eq!(hiragana_to_romaji("かきくけこ"), "kakikukeko");
    }

    #[test]
    fn test_reverse_romaji_shi_chi_tsu() {
        assert_eq!(hiragana_to_romaji("し"), "shi");
        assert_eq!(hiragana_to_romaji("ち"), "chi");
        assert_eq!(hiragana_to_romaji("つ"), "tsu");
        assert_eq!(hiragana_to_romaji("ふ"), "fu");
    }

    #[test]
    fn test_reverse_romaji_youon() {
        assert_eq!(hiragana_to_romaji("しゃ"), "sha");
        assert_eq!(hiragana_to_romaji("ちゅ"), "chu");
        assert_eq!(hiragana_to_romaji("にょ"), "nyo");
    }

    #[test]
    fn test_reverse_romaji_nn() {
        assert_eq!(hiragana_to_romaji("ん"), "nn");
        assert_eq!(hiragana_to_romaji("にほんご"), "nihonngo");
    }

    #[test]
    fn test_reverse_romaji_sokuon() {
        assert_eq!(hiragana_to_romaji("っ"), "xtu");
    }

    #[test]
    fn test_reverse_romaji_punctuation() {
        assert_eq!(hiragana_to_romaji("、"), ",");
        assert_eq!(hiragana_to_romaji("。"), ".");
        assert_eq!(hiragana_to_romaji("ー"), "-");
    }

    #[test]
    fn test_reverse_romaji_passthrough() {
        // Non-kana characters pass through unchanged
        assert_eq!(hiragana_to_romaji("abc"), "abc");
        assert_eq!(hiragana_to_romaji("あbc"), "abc");
    }

    #[test]
    fn test_karukan_key_from_u32_new_keys() {
        assert_eq!(KarukanKey::from_u32(10), Some(KarukanKey::ConvertHiragana));
        assert_eq!(KarukanKey::from_u32(11), Some(KarukanKey::ConvertKatakana));
        assert_eq!(KarukanKey::from_u32(12), Some(KarukanKey::ConvertAscii));
    }

    // ── composing_hiragana tests ──

    #[test]
    fn test_composing_hiragana_returns_text() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.composing_hiragana(), Some("あいう"));
    }

    #[test]
    fn test_composing_hiragana_none_when_empty_state() {
        let s = KarukanSession::new();
        assert_eq!(s.composing_hiragana(), None);
    }

    #[test]
    fn test_composing_hiragana_none_when_input_buf_empty() {
        let mut s = KarukanSession::new();
        // "k" だけでは input_buf は空（romaji バッファにのみ存在）
        s.push_char('k');
        // input_buf.text は空なので composing_hiragana は None
        assert_eq!(s.composing_hiragana(), None);
    }

    #[test]
    fn test_composing_hiragana_none_after_commit() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Return);
        assert!(s.is_empty());
        assert_eq!(s.composing_hiragana(), None);
    }

    // ── apply_live_candidate tests ──

    #[test]
    fn test_apply_live_candidate_basic() {
        let mut s = KarukanSession::new();
        "nadesi".chars().for_each(|c| { s.push_char(c); });
        assert_eq!(s.preedit.text.to_str().unwrap(), "なでし");

        s.apply_live_candidate("撫子", "なでし");

        assert!(s.preedit.dirty);
        assert_eq!(s.preedit.text.to_str().unwrap(), "撫子");
        assert!(!s.is_empty()); // Composing のまま
        assert!(!s.commit.dirty);
    }

    #[test]
    fn test_apply_live_candidate_stale_source_ignored() {
        let mut s = KarukanSession::new();
        "nadesi".chars().for_each(|c| { s.push_char(c); });

        // source が現在の input_buf.text ("なでし") と不一致 → 無視
        s.apply_live_candidate("撫子", "なで");
        // preedit は変わらない
        assert_eq!(s.preedit.text.to_str().unwrap(), "なでし");
    }

    #[test]
    fn test_apply_live_candidate_not_composing_ignored() {
        let mut s = KarukanSession::new();
        assert!(s.is_empty());
        // Empty 状態 → 無視される
        s.apply_live_candidate("撫子", "");
        assert!(s.is_empty());
        assert!(!s.preedit.dirty);
    }

    #[test]
    fn test_apply_live_candidate_includes_romaji_buffer() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // input_buf = "あ"
        s.push_char('k'); // romaji buffer = "k"

        s.apply_live_candidate("亜", "あ");
        // preedit = "亜" + "k"
        assert_eq!(s.preedit.text.to_str().unwrap(), "亜k");
    }

    #[test]
    fn test_backspace_clears_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        s.apply_live_candidate("愛憂", "あいう");
        assert_eq!(s.preedit.text.to_str().unwrap(), "愛憂");

        s.push_key(KarukanKey::Backspace);
        // live_candidate がクリアされ、ひらがな表示 ("あい") に戻る
        assert_eq!(s.preedit.text.to_str().unwrap(), "あい");
    }

    #[test]
    fn test_commit_uses_live_candidate_when_source_matches() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        s.apply_live_candidate("愛憂", "あいう");

        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "愛憂");
        assert!(s.is_empty());
    }

    #[test]
    fn test_commit_ignores_stale_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        // source が stale → apply は no-op
        s.apply_live_candidate("愛憂", "あい");
        assert_eq!(s.preedit.text.to_str().unwrap(), "あいう");

        // Return → ひらがなをコミット
        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "あいう");
    }

    #[test]
    fn test_escape_two_step_with_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        s.apply_live_candidate("愛憂", "あいう");

        // 1 回目 Escape: live_candidate のみクリア
        s.push_key(KarukanKey::Escape);
        assert!(!s.is_empty()); // まだ Composing
        assert_eq!(s.preedit.text.to_str().unwrap(), "あいう");

        // 2 回目 Escape: 全キャンセル
        s.push_key(KarukanKey::Escape);
        assert!(s.is_empty());
        assert!(!s.commit.dirty);
    }

    // ── set_composing_hiragana tests ──

    #[test]
    fn test_set_composing_hiragana_from_empty() {
        let mut s = KarukanSession::new();
        assert!(s.is_empty());
        s.set_composing_hiragana("かきく");
        assert!(!s.is_empty());
        assert_eq!(s.preedit.text.to_str().unwrap(), "かきく");
        // cursor は末尾 ("かきく" = 9 bytes)
        assert_eq!(s.preedit.caret_bytes, 9);
    }

    #[test]
    fn test_set_composing_hiragana_overwrites_existing() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // "あ"
        s.set_composing_hiragana("なでしこ");
        assert_eq!(s.preedit.text.to_str().unwrap(), "なでしこ");
    }

    #[test]
    fn test_set_composing_hiragana_empty_is_noop() {
        let mut s = KarukanSession::new();
        s.set_composing_hiragana(""); // no-op
        assert!(s.is_empty());
    }

    #[test]
    fn test_set_composing_hiragana_resets_romaji() {
        let mut s = KarukanSession::new();
        s.push_char('k'); // romaji buffer に "k" が残る
        assert!(s.is_consonant_pending());
        s.set_composing_hiragana("さくら");
        assert!(!s.is_consonant_pending()); // romaji バッファがクリアされた
        assert_eq!(s.preedit.text.to_str().unwrap(), "さくら");
    }

    #[test]
    fn test_set_composing_hiragana_clears_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| { s.push_char(c); });
        s.apply_live_candidate("愛憂", "あいう");
        assert_eq!(s.preedit.text.to_str().unwrap(), "愛憂");

        // set_composing_hiragana は live_candidate もクリアする
        s.set_composing_hiragana("こんにちは");
        assert_eq!(s.preedit.text.to_str().unwrap(), "こんにちは");
        // 次の Return はひらがなをコミットする (live_candidate ではない)
        s.push_key(KarukanKey::Return);
        assert_eq!(s.commit.text.to_str().unwrap(), "こんにちは");
    }

    // ── select_candidate 境界値 ──

    #[test]
    fn test_select_candidate_out_of_range() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space); // → Conversion
        assert!(!s.select_candidate(999)); // 範囲外 → false
        assert!(!s.is_empty()); // まだ Conversion
    }

    #[test]
    fn test_select_candidate_not_in_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // Composing 状態
        assert!(!s.select_candidate(0)); // Conversion でない → false
    }
}
