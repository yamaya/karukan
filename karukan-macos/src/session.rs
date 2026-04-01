//! IME session state machine for macOS.
//!
//! `KarukanSession` wraps `karukan-engine` primitives (`RomajiConverter`,
//! `LearningCache`, `KanaKanjiConverter`) and implements the state machine:
//!
//! ```text
//! Empty ──push_char──→ Composing
//! Composing ──Return──→ Empty              (commit hiragana)
//! Composing ──Escape──→ Empty              (cancel)
//! Composing ──Backspace──→ Composing | Empty
//! Composing ──Space──→ BunsetsuConversion  (kanji/bunsetsu conversion)
//! Composing(live) ──Left──→ BunsetsuConversion (enter at last segment)
//! BunsetsuConversion ──Return──→ Empty     (commit all segments)
//! BunsetsuConversion ──Escape──→ Composing (cancel)
//! BunsetsuConversion ──Left──→ BunsetsuConversion (prev segment)
//! BunsetsuConversion ──Right──→ BunsetsuConversion (next segment)
//! BunsetsuConversion ──Space──→ BunsetsuConversion (show candidates)
//! ```

use std::ffi::CString;
use std::sync::{Arc, OnceLock};

use karukan_engine::{
    Backend, BackspaceResult, KanaKanjiConverter, LearningCache, RomajiConverter,
};

// ---------------------------------------------------------------------------
// Process-wide shared KanaKanjiConverter
// ---------------------------------------------------------------------------

/// Loaded at most once per process; all `KarukanSession` instances share it.
///
/// `OnceLock` provides lock-free reads after the first init. `Arc` lets every
/// session hold a cheap reference without copying the model.
pub(crate) static SHARED_CONVERTER: OnceLock<Arc<KanaKanjiConverter>> = OnceLock::new();

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
    ShrinkSegment = 13,
    ExtendSegment = 14,
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
            13 => Some(Self::ShrinkSegment),
            14 => Some(Self::ExtendSegment),
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

/// 一つの文節（変換単位）。
struct BunsetsuSegment {
    /// 文節の読み（ひらがな）。Escape 時の復元・学習記録に使う。
    hiragana: String,
    /// 現在の表示テキスト（候補リストの先頭、または選択済み候補）。
    display: String,
    /// 変換候補リスト（Learning → User Dict → Model → System Dict）。
    candidates: Vec<String>,
}

/// 文節変換モードの状態。
struct BunsetsuConversionState {
    /// 文節リスト（1 つ以上）。
    segments: Vec<BunsetsuSegment>,
    /// 現在選択中の文節インデックス。
    selected: usize,
}

enum SessionState {
    /// No pending input.
    Empty,
    /// Accumulating romaji / hiragana input.
    Composing,
    /// 文節変換中（候補パネルの表示は candidate_cache で制御）。
    BunsetsuConversion(BunsetsuConversionState),
}

/// Ctrl+J/K/; プレビューモード。
///
/// ライブ変換中に Ctrl+J/K/; を初めて押すとプレビュー表示になり、
/// 同じキーをもう一度押すと確定する。別のキーを押すとモードが切り替わる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConvertPreview {
    Hiragana,
    Katakana,
    Ascii,
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
    /// Optional user dictionary (loaded by `init_resources` from `user_dicts/`).
    user_dict: Option<karukan_engine::Dictionary>,
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
    /// Ctrl+J/K/; のプレビュー状態。
    /// ライブ変換中に Ctrl+J/K/; を押すとまずプレビュー表示し、同じキーをもう一度押すと確定する。
    convert_preview: Option<ConvertPreview>,
    /// Preedit state exposed to FFI.
    pub(crate) preedit: PreeditCache,
    /// Commit state exposed to FFI.
    pub(crate) commit: CommitCache,
    /// Candidate list exposed to FFI.
    pub(crate) candidate_cache: CandidateCache,
    /// Override data directory (for tests). When `Some`, paths are resolved
    /// relative to this directory instead of `platform::paths`.
    data_dir: Option<std::path::PathBuf>,
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
            user_dict: None,
            learning: None,
            converter: None,
            live_candidate: None,
            live_candidate_source: String::new(),
            convert_preview: None,
            preedit: PreeditCache::default(),
            commit: CommitCache::default(),
            candidate_cache: CandidateCache::default(),
            data_dir: None,
        }
    }

    /// Load resources from disk (dictionary, learning cache, kanji model).
    ///
    /// Missing files are silently skipped (non-fatal).
    /// Call from a background thread — model download may take time on first run.
    pub fn init_resources(&mut self) {
        use crate::platform::paths;

        let base_dir = self.data_dir.clone();

        // System dictionary (optional — may not exist yet)
        let dict_path = base_dir
            .as_ref()
            .map(|d| d.join("dict.bin"))
            .unwrap_or_else(|| paths::system_dict_path());
        if dict_path.exists() {
            match karukan_engine::Dictionary::load(&dict_path) {
                Ok(d) => {
                    tracing::info!("Loaded system dictionary from {:?}", dict_path);
                    self.dict = Some(d);
                }
                Err(e) => tracing::warn!("Failed to load system dictionary: {}", e),
            }
        }

        // User dictionaries (optional — scan user_dicts/ directories)
        // In an App Sandbox the container path and the real path differ;
        // scan both so that dictionaries placed at either location are found.
        let search_dirs: Vec<std::path::PathBuf> = if let Some(ref d) = base_dir {
            vec![d.join("user_dicts")]
        } else {
            paths::user_dict_dirs()
        };

        let mut dicts = Vec::new();
        let mut loaded_files = std::collections::HashSet::new();
        for user_dict_dir in &search_dirs {
            if !user_dict_dir.exists() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(user_dict_dir) else {
                continue;
            };
            let mut file_paths: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            file_paths.sort();

            for path in &file_paths {
                // ファイル名で重複を排除（コンテナ内と実パスの両方に同じファイルがある場合）
                let file_name = path.file_name().unwrap_or_default().to_owned();
                if !loaded_files.insert(file_name) {
                    tracing::info!("Skipping duplicate user dictionary {:?}", path);
                    continue;
                }
                match karukan_engine::Dictionary::load_auto(path) {
                    Ok(dict) => {
                        tracing::info!("Loaded user dictionary from {:?}", path);
                        dicts.push(dict);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load user dictionary {:?}: {}", path, e)
                    }
                }
            }
        }

        if !dicts.is_empty() {
            match karukan_engine::Dictionary::merge(dicts) {
                Ok(Some(merged)) => {
                    tracing::info!("User dictionaries merged ({} files)", loaded_files.len());
                    self.user_dict = Some(merged);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Failed to merge user dictionaries: {}", e),
            }
        }

        // Learning cache (create empty if file doesn't exist yet)
        let learning_path = base_dir
            .as_ref()
            .map(|d| d.join("learning.tsv"))
            .unwrap_or_else(|| paths::learning_cache_path());
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
                .and_then(|backend| KanaKanjiConverter::new(backend).map_err(|e| e.to_string()))
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

        let path = self
            .data_dir
            .as_ref()
            .map(|d| d.join("learning.tsv"))
            .unwrap_or_else(|| crate::platform::paths::learning_cache_path());
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

    /// pending romaji バッファのバイト長を返す。
    /// pending がなければ 0。Swift が dotted underline の範囲計算に使う。
    pub fn romaji_buf_len(&self) -> usize {
        self.romaji.buffer().len()
    }

    /// BunsetsuConversion 状態の文節数を返す。それ以外の状態では 0。
    pub fn segment_count(&self) -> usize {
        match &self.state {
            SessionState::BunsetsuConversion(conv) => conv.segments.len(),
            _ => 0,
        }
    }

    /// BunsetsuConversion 状態の選択文節インデックスを返す。それ以外では 0。
    pub fn selected_segment(&self) -> usize {
        match &self.state {
            SessionState::BunsetsuConversion(conv) => conv.selected,
            _ => 0,
        }
    }

    /// BunsetsuConversion 状態の文節 `index` の現在表示テキストの文字数（NSString 長）を返す。
    /// それ以外の状態またはインデックス範囲外では 0。
    pub fn segment_char_count(&self, index: usize) -> usize {
        match &self.state {
            SessionState::BunsetsuConversion(conv) => conv
                .segments
                .get(index)
                .map(|s| s.display.chars().count())
                .unwrap_or(0),
            _ => 0,
        }
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
    ///
    /// モデル結果よりも学習キャッシュ・ユーザー辞書を優先する。
    /// 優先度: Learning → User Dictionary → Model
    pub fn apply_live_candidate(&mut self, candidate: &str, source: &str) {
        if !matches!(self.state, SessionState::Composing) {
            return;
        }
        // source が現在の input_buf.text と異なる場合は stale — 無視する。
        // romaji バッファに未確定子音がある場合も stale として扱う:
        //   `s` 入力後は input_buf.text は変化しないが、有効な入力は変わっている。
        //   auto-commit が pending 子音を失うのを防ぐ。
        if source != self.input_buf.text || !self.romaji.buffer().is_empty() {
            // preedit.dirty を false にリセットして FFI が 0 を返すようにする。
            // これにより Swift 側の auto-commit path が発火しない。
            // （push_char が設定した preedit.dirty = true が残ると、FFI が 1 を返して
            //   auto-commit が誤トリガーされる。）
            self.preedit.dirty = false;
            return;
        }
        self.clear_flags();

        // Learning → User Dictionary → Model の優先度で候補を決定。
        let effective = self
            .lookup_live_override(source)
            .unwrap_or_else(|| candidate.to_string());
        // 制御文字を除去する。全文字が除去された場合は raw ひらがな（source）にフォールバック。
        // preedit.text に制御文字が残ると setMarkedText("0x10...") → setMarkedText("") の連鎖で
        // 一部のアプリが制御文字を commit してしまうため。
        let effective: String = effective.chars().filter(|c| !c.is_control()).collect();
        let effective = if effective.is_empty() {
            source.to_string()
        } else {
            effective
        };
        self.live_candidate = Some(effective.clone());
        self.live_candidate_source = source.to_string();
        // preedit = 変換済みテキスト + 未確定ローマ字バッファ
        // 例: candidate="日本語", romaji.buffer()="h" → preedit="日本語h"
        // karukan-im の set_composing_state() が live.text + romaji buffer を合成するのと同じ。
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = format!("{}{}", effective, romaji_buf);
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
        self.convert_preview = None;

        // ライブ変換結果が残っていれば、推論完了まで表示ベースとして使い続ける。
        // clone() で参照し live_candidate は保持する。apply_live_candidate() が
        // 新しい結果で上書きするか、do_commit/do_cancel で消費される。
        // take() にすると連続キー入力で None になりひらがなフォールバックが起きる。
        let prev_live = self.live_candidate.clone();

        // In BunsetsuConversion state, any printable char cancels conversion and
        // re-enters Composing (commit the char as new input).
        if matches!(self.state, SessionState::BunsetsuConversion(_)) {
            self.cancel_bunsetsu();
        }

        // Track the previous output length so we can compute the delta.
        let prev_output_chars = self.romaji.output().chars().count();

        // Feed the character to the romaji converter.
        let _event = self.romaji.push(ch);

        // Append only the newly produced hiragana to input_buf.
        // ASCII数字は全角数字に変換する（ロマジコンバータはルールがなくPassThroughする）。
        let new_hiragana: String = self
            .romaji
            .output()
            .chars()
            .skip(prev_output_chars)
            .map(|c| match c {
                '0'..='9' => char::from_u32(c as u32 - '0' as u32 + '０' as u32).unwrap(),
                _ => c,
            })
            .collect();
        if !new_hiragana.is_empty() {
            self.input_buf.insert(&new_hiragana);
        }

        // Build preedit:
        // prev_live がある場合（例: "今日"）はそれをベースにする。
        // live_candidate_source 以降に追加された全文字（tail）を付加する。
        //   例: live="今日", source="きょう", input_buf="きょうは" → tail="は"
        //        preedit = "今日" + "は" + romaji_buf
        // new_hiragana（今回の1打鍵分のデルタ）ではなく tail を使うことで、
        // 高速入力時に live_candidate 更新前に複数回 push_char が呼ばれても
        // 中間のかなが欠落しない。
        // これにより文字数が一時的に減る中間状態を避け、
        // カーソルの前後ジャンプを防ぐ。推論完了後は apply_live_candidate が上書きする。
        // prev_live がなければ従来通り input_buf ひらがな + ローマ字バッファ。
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = if let Some(ref live) = prev_live {
            if self.input_buf.text.starts_with(&self.live_candidate_source) {
                let tail = &self.input_buf.text[self.live_candidate_source.len()..];
                format!("{}{}{}", live, tail, romaji_buf)
            } else {
                // source がプレフィクスでない（stale）→ ひらがなフォールバック
                format!("{}{}", self.input_buf.text, romaji_buf)
            }
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
            // ── Empty state ───────────────────────────────────────────────
            // Space → 全角スペース (U+3000) をコミット（preedit なし時の標準日本語 IME 動作）。
            // その他のキーはアプリにパススルー。
            KarukanKey::Space if matches!(self.state, SessionState::Empty) => {
                self.convert_preview = None;
                self.commit.text = CString::new("\u{3000}").unwrap_or_default();
                self.commit.dirty = true;
                true
            }
            _ if matches!(self.state, SessionState::Empty) => {
                self.convert_preview = None;
                false
            }

            // ── BunsetsuConversion state で Ctrl+K/J → 選択文節のみ変換 ──────────
            KarukanKey::ConvertKatakana
                if matches!(self.state, SessionState::BunsetsuConversion(_)) =>
            {
                self.do_convert_segment_katakana();
                true
            }
            KarukanKey::ConvertHiragana
                if matches!(self.state, SessionState::BunsetsuConversion(_)) =>
            {
                self.do_convert_segment_hiragana();
                true
            }

            // ── Convert shortcuts (Composing + Conversion) ───────────────
            // convert_preview は do_convert 内で管理する
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
                let had_live = self.live_candidate.is_some();
                self.do_conversion_impl(false);
                // ライブ変換中は既に変換結果が表示されているので、即座に候補パネルを表示する
                if had_live {
                    self.show_segment_candidates();
                }
                true
            }
            // ライブ変換中に Left: 最後の文節を選択した状態で文節変換に入る
            KarukanKey::Left
                if matches!(self.state, SessionState::Composing)
                    && self.live_candidate.is_some() =>
            {
                self.do_conversion_impl(true);
                self.show_segment_candidates();
                true
            }

            // ── BunsetsuConversion state ──────────────────────────────────
            KarukanKey::Return if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                self.commit_bunsetsu_all();
                true
            }
            KarukanKey::Escape if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                if !self.candidate_cache.items.is_empty() {
                    // 候補パネル表示中: パネルを隠すだけ（文節ナビを継続）
                    self.candidate_cache.items.clear();
                    self.candidate_cache.cursor = 0;
                } else {
                    // パネル非表示: 変換キャンセル → Composing
                    self.cancel_bunsetsu();
                }
                true
            }
            KarukanKey::Backspace if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                self.cancel_bunsetsu();
                self.do_backspace();
                true
            }
            KarukanKey::Left if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                self.move_segment(-1);
                true
            }
            KarukanKey::Right if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                self.move_segment(1);
                true
            }
            KarukanKey::Space | KarukanKey::Tab | KarukanKey::Down
                if matches!(self.state, SessionState::BunsetsuConversion(_)) =>
            {
                // Swift がパネル表示中に Space/Tab/Down を先に処理するため、
                // ここに来るのはパネル非表示時のみ。
                self.show_segment_candidates();
                true
            }
            KarukanKey::Up if matches!(self.state, SessionState::BunsetsuConversion(_)) => {
                // パネル表示中は Swift/IMKCandidates が処理。消費だけする。
                true
            }
            KarukanKey::ShrinkSegment
                if matches!(self.state, SessionState::BunsetsuConversion(_)) =>
            {
                self.resize_segment(-1);
                true
            }
            KarukanKey::ExtendSegment
                if matches!(self.state, SessionState::BunsetsuConversion(_)) =>
            {
                self.resize_segment(1);
                true
            }

            _ => false,
        }
    }

    /// Select a candidate by index.
    ///
    /// Used by `candidateSelected(_:)` in Swift (IMKCandidates click / Return).
    ///
    /// BunsetsuConversion 状態では選択文節の display を更新し、コミットはしない
    /// (`commit.dirty` を立てない)。Swift 側が `karukan_has_commit() == 0` を
    /// 確認して preedit を更新するだけにとどめる。
    ///
    /// Returns `true` on success, `false` if not in a conversion state or out of range.
    pub fn select_candidate(&mut self, index: usize) -> bool {
        self.clear_flags();
        let SessionState::BunsetsuConversion(ref conv) = self.state else {
            return false;
        };
        let sel = conv.selected;
        if sel >= conv.segments.len() || index >= conv.segments[sel].candidates.len() {
            return false;
        }
        let chosen = conv.segments[sel].candidates[index].clone();

        // display を更新してから全文節をコミット。
        // 学習記録は commit_bunsetsu_all が担う（display != hiragana の全文節を記録）。
        if let SessionState::BunsetsuConversion(ref mut conv) = self.state {
            conv.segments[sel].display = chosen;
        }
        self.commit_bunsetsu_all();
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
        self.convert_preview = None;

        // flush 前の input_buf.text を保存。live_candidate_source との比較に使う。
        // flush で子音が pass through されると input_buf.text が変化し、
        // live_candidate_source と不一致になって live が不採用 → ASCII 混在コミットになるため。
        let pre_flush_text = self.input_buf.text.clone();

        let prev_len = self.romaji.output().chars().count();
        let _ = self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }

        let committed = if let Some(live) = self.live_candidate.take() {
            // live_candidate_source が flush 前の input_buf.text と一致する場合のみ採用。
            // 'k' 押下時（composing="なでし"）に生成された live_candidate が
            // 'o' 入力後（composing="なでしこ"）にコミットされるバグを防ぐ。
            if self.live_candidate_source == pre_flush_text {
                if let Some(cache) = &mut self.learning {
                    cache.record(&pre_flush_text, &live);
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

        // 制御文字を除去（学習キャッシュ汚染や予期せぬモデル出力からの防御）。
        let committed: String = committed.chars().filter(|c| !c.is_control()).collect();
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
        self.convert_preview = None;
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
        self.convert_preview = None;
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

    /// Restore hiragana from BunsetsuConversion state before converting.
    ///
    /// If in BunsetsuConversion state, restores `input_buf` from the saved
    /// hiragana and transitions to Composing so that `do_convert_*` can operate.
    fn restore_hiragana_if_conversion(&mut self) {
        let hiragana: Option<String> = match &self.state {
            SessionState::BunsetsuConversion(conv) => {
                Some(conv.segments.iter().map(|s| s.hiragana.as_str()).collect())
            }
            _ => None,
        };
        if let Some(hiragana) = hiragana {
            self.candidate_cache.items.clear();
            self.candidate_cache.cursor = 0;
            self.input_buf.clear();
            self.input_buf.insert(&hiragana);
            self.romaji.reset();
            self.state = SessionState::Composing;
        }
    }

    /// Ctrl+J/K/; 共通: プレビュー → 確定の2段階処理。
    ///
    /// - ライブ変換中 or 別モードのプレビュー中 → プレビュー表示のみ（確定しない）
    /// - 同じモードのプレビュー中 or ライブ変換なし → 確定
    fn do_convert(&mut self, mode: ConvertPreview) {
        self.restore_hiragana_if_conversion();

        // ライブ変換中 or 別モードプレビュー中 → プレビュー表示のみ
        let should_preview =
            self.live_candidate.is_some() || matches!(self.convert_preview, Some(m) if m != mode);

        if should_preview {
            self.live_candidate = None;
            self.convert_preview = Some(mode);
            let preedit_text = match mode {
                ConvertPreview::Hiragana => {
                    format!("{}{}", self.input_buf.text, self.romaji.buffer())
                }
                ConvertPreview::Katakana => {
                    let katakana = karukan_engine::kana::hiragana_to_katakana(&self.input_buf.text);
                    format!("{}{}", katakana, self.romaji.buffer())
                }
                ConvertPreview::Ascii => {
                    let romaji = hiragana_to_romaji(&self.input_buf.text);
                    format!("{}{}", romaji, self.romaji.buffer())
                }
            };
            self.update_preedit(&preedit_text);
            return;
        }

        // 同じモード2回目 or ライブ変換なし → 確定
        self.convert_preview = None;
        self.flush_romaji();
        if self.input_buf.text.is_empty() {
            return;
        }

        let committed = match mode {
            ConvertPreview::Hiragana => std::mem::take(&mut self.input_buf.text),
            ConvertPreview::Katakana => {
                karukan_engine::kana::hiragana_to_katakana(&self.input_buf.text)
            }
            ConvertPreview::Ascii => hiragana_to_romaji(&self.input_buf.text),
        };
        self.live_candidate = None;
        self.input_buf.clear();
        self.romaji.reset();
        self.state = SessionState::Empty;
        self.commit.text = CString::new(committed).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    /// Ctrl+J: ひらがなのまま確定（ライブ変換中はプレビュー→確定の2段階）。
    fn do_convert_hiragana(&mut self) {
        self.do_convert(ConvertPreview::Hiragana);
    }

    /// Ctrl+K: カタカナに変換して確定（ライブ変換中はプレビュー→確定の2段階）。
    fn do_convert_katakana(&mut self) {
        self.do_convert(ConvertPreview::Katakana);
    }

    /// Ctrl+;: 半角英数に逆変換して確定（ライブ変換中はプレビュー→確定の2段階）。
    fn do_convert_ascii(&mut self) {
        self.do_convert(ConvertPreview::Ascii);
    }

    /// 文節変換中に Ctrl+K: 選択文節のみカタカナに変換して display に設定（確定しない）。
    fn do_convert_segment_katakana(&mut self) {
        self.do_convert_segment_kana(false);
    }

    /// 文節変換中に Ctrl+J: 選択文節の display を元のひらがな読みに戻す（確定しない）。
    fn do_convert_segment_hiragana(&mut self) {
        self.do_convert_segment_kana(true);
    }

    fn do_convert_segment_kana(&mut self, to_hiragana: bool) {
        let SessionState::BunsetsuConversion(ref mut conv) = self.state else {
            return;
        };
        let sel = conv.selected;
        if let Some(seg) = conv.segments.get_mut(sel) {
            seg.display = if to_hiragana {
                seg.hiragana.clone()
            } else {
                karukan_engine::kana::hiragana_to_katakana(&seg.hiragana)
            };
        }
        let preedit_text: String = conv.segments.iter().map(|s| s.display.as_str()).collect();
        let caret_bytes: usize = conv.segments[..=sel].iter().map(|s| s.display.len()).sum();
        self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
        self.preedit.caret_bytes = caret_bytes as u32;
        self.preedit.dirty = true;
        self.candidate_cache.items.clear();
    }

    // -----------------------------------------------------------------------
    // Private helpers — Conversion
    // -----------------------------------------------------------------------

    /// 文節変換を開始する。
    ///
    /// `start_at_last = true` のとき最後の文節を選択状態にする
    /// （ライブ変換中の Left キー用）。`false` のとき最初の文節を選択。
    fn do_conversion_impl(&mut self, start_at_last: bool) {
        self.convert_preview = None;
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

        // ライブ変換結果を取り出す（文節変換移行前にクリア）
        let prev_live = self.live_candidate.take();

        // ひらがなを文節に分割する。候補は lazy loading（show_segment_candidates で遅延ロード）。
        let hiragana_segs = segment_hiragana(&hiragana);
        let mut segments: Vec<BunsetsuSegment> = hiragana_segs
            .iter()
            .map(|h| BunsetsuSegment {
                hiragana: h.clone(),
                display: h.clone(), // 初期表示 = ひらがな（候補ロード前）
                candidates: vec![], // 空: show_segment_candidates で遅延ロード
            })
            .collect();

        // 単一文節かつ prev_live がある場合: display にライブ変換結果を使う
        // （candidates は空のまま; ロード時に学習キャッシュから先頭に追加される）
        if segments.len() == 1 {
            if let Some(live) = prev_live {
                if live != hiragana {
                    segments[0].display = live;
                }
            }
        }

        let n = segments.len();
        let initial_selected = if start_at_last {
            n.saturating_sub(1)
        } else {
            0
        };

        // candidate_cache は空（1 回目の Space では候補パネルを表示しない）。
        // 2 回目の Space / Down で show_segment_candidates が lazy ロードして表示する。
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;

        // preedit = 全文節の display を連結（キャレット = 選択文節末尾）
        let preedit_text: String = segments.iter().map(|s| s.display.as_str()).collect();
        let caret_bytes: usize = segments[..=initial_selected]
            .iter()
            .map(|s| s.display.len())
            .sum();
        self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
        self.preedit.caret_bytes = caret_bytes as u32;
        self.preedit.dirty = true;

        self.state = SessionState::BunsetsuConversion(BunsetsuConversionState {
            segments,
            selected: initial_selected,
        });
    }

    /// 選択文節を delta だけ移動する（-1 = 前、+1 = 次）。端でクランプ。
    fn move_segment(&mut self, delta: i32) {
        let (new_selected, preedit_text, caret_bytes) = match &mut self.state {
            SessionState::BunsetsuConversion(conv) => {
                let n = conv.segments.len();
                if n == 0 {
                    return;
                }
                let new_sel = (conv.selected as i32 + delta).clamp(0, n as i32 - 1) as usize;
                conv.selected = new_sel;
                let text: String = conv.segments.iter().map(|s| s.display.as_str()).collect();
                let caret: usize = conv.segments[..=new_sel]
                    .iter()
                    .map(|s| s.display.len())
                    .sum();
                (new_sel, text, caret)
            }
            _ => return,
        };
        let _ = new_selected; // used above via conv.selected
        // 候補パネルを隠す
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
        self.preedit.caret_bytes = caret_bytes as u32;
        self.preedit.dirty = true;
    }

    /// 選択文節の境界を1文字分変更する（文節伸縮）。
    ///
    /// `delta > 0` (ExtendSegment / Ctrl+O / Shift+Right):
    ///   次の文節の先頭1文字を現在の文節末尾に追加する。
    ///   次の文節が空になった場合は削除する。
    ///
    /// `delta < 0` (ShrinkSegment / Ctrl+I / Shift+Left):
    ///   現在の文節の末尾1文字を取り出し、新しい独立した文節として sel+1 に挿入する。
    ///   後続の文節はそのまま後ろへずれる（内容は変更しない）。
    ///   現在の文節が1文字以下なら何もしない（最小1文字制約）。
    fn resize_segment(&mut self, delta: i32) {
        let (sel, preedit_text, caret_bytes) = match &mut self.state {
            SessionState::BunsetsuConversion(conv) => {
                let sel = conv.selected;
                if delta > 0 {
                    // 延ばす: 次の文節の先頭1文字を現在の文節末尾へ
                    if sel + 1 >= conv.segments.len() {
                        return;
                    }
                    let ch = match conv.segments[sel + 1].hiragana.chars().next() {
                        Some(c) => c,
                        None => return,
                    };
                    conv.segments[sel].hiragana.push(ch);
                    conv.segments[sel].display = conv.segments[sel].hiragana.clone();
                    conv.segments[sel].candidates.clear();

                    let remaining: String =
                        conv.segments[sel + 1].hiragana.chars().skip(1).collect();
                    if remaining.is_empty() {
                        conv.segments.remove(sel + 1);
                    } else {
                        conv.segments[sel + 1].hiragana = remaining.clone();
                        conv.segments[sel + 1].display = remaining;
                        conv.segments[sel + 1].candidates.clear();
                    }
                } else {
                    // 縮める: 現在の文節の末尾1文字を次の文節先頭へ
                    if conv.segments[sel].hiragana.chars().count() <= 1 {
                        return;
                    }
                    let popped = conv.segments[sel].hiragana.chars().last().unwrap();
                    let new_current: String = {
                        let mut s = conv.segments[sel].hiragana.clone();
                        s.pop();
                        s
                    };
                    conv.segments[sel].hiragana = new_current.clone();
                    conv.segments[sel].display = new_current;
                    conv.segments[sel].candidates.clear();

                    let mut popped_str = String::new();
                    popped_str.push(popped);
                    // 常に新しい独立した文節として挿入する。
                    // 後続の文節はそのまま後ろへずれ、内容は変更しない。
                    conv.segments.insert(
                        sel + 1,
                        BunsetsuSegment {
                            hiragana: popped_str.clone(),
                            display: popped_str,
                            candidates: vec![],
                        },
                    );
                }

                let text: String = conv.segments.iter().map(|s| s.display.as_str()).collect();
                let caret: usize = conv.segments[..=sel].iter().map(|s| s.display.len()).sum();
                (sel, text, caret)
            }
            _ => return,
        };
        let _ = sel;
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
        self.preedit.caret_bytes = caret_bytes as u32;
        self.preedit.dirty = true;
    }

    /// 選択文節の候補を candidate_cache に設定する（候補パネルを表示）。
    ///
    /// 候補が未ロードの場合（candidates が空）は `collect_candidates` を呼んで
    /// lazy ロードする。display もひらがなのままなら最良候補に更新し preedit を刷新する。
    fn show_segment_candidates(&mut self) {
        // lazy loading: 選択文節の候補が未ロードなら今ロードする
        let needs_load = match &self.state {
            SessionState::BunsetsuConversion(conv) => conv
                .segments
                .get(conv.selected)
                .map_or(false, |s| s.candidates.is_empty()),
            _ => return,
        };

        if needs_load {
            let (selected, hiragana) = match &self.state {
                SessionState::BunsetsuConversion(conv) => (
                    conv.selected,
                    conv.segments
                        .get(conv.selected)
                        .map(|s| s.hiragana.clone())
                        .unwrap_or_default(),
                ),
                _ => return,
            };
            // collect_candidates は &self を借用するため、state の可変借用の前に完了させる
            let candidates = self.collect_candidates(&hiragana);

            if let SessionState::BunsetsuConversion(ref mut conv) = self.state {
                if let Some(seg) = conv.segments.get_mut(selected) {
                    seg.candidates = candidates;
                    // display がまだひらがなの場合: 最良候補に更新
                    if seg.display == seg.hiragana {
                        if let Some(first) = seg.candidates.first() {
                            seg.display = first.clone();
                        }
                    }
                }
                // preedit を更新（display が変わった可能性があるため）
                let preedit_text: String =
                    conv.segments.iter().map(|s| s.display.as_str()).collect();
                let caret_bytes: usize = conv.segments[..=conv.selected]
                    .iter()
                    .map(|s| s.display.len())
                    .sum();
                self.preedit.text = CString::new(preedit_text.as_str()).unwrap_or_default();
                self.preedit.caret_bytes = caret_bytes as u32;
                self.preedit.dirty = true;
            }
        }

        // candidate_cache を選択文節の候補で埋める（パネル表示トリガー）
        let candidates: Vec<CString> = match &self.state {
            SessionState::BunsetsuConversion(conv) => conv
                .segments
                .get(conv.selected)
                .map(|s| {
                    s.candidates
                        .iter()
                        .map(|c| CString::new(c.as_str()).unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default(),
            _ => return,
        };
        self.candidate_cache.items = candidates;
        self.candidate_cache.cursor = 0;
    }

    /// 全文節を結合してコミットし、Empty 状態に戻る。
    fn commit_bunsetsu_all(&mut self) {
        let (committed, learning_entries) = match &self.state {
            SessionState::BunsetsuConversion(conv) => {
                let text: String = conv.segments.iter().map(|s| s.display.as_str()).collect();
                let entries: Vec<(String, String)> = conv
                    .segments
                    .iter()
                    .filter(|s| s.display != s.hiragana)
                    .map(|s| (s.hiragana.clone(), s.display.clone()))
                    .collect();
                (text, entries)
            }
            _ => return,
        };
        for (hiragana, display) in &learning_entries {
            if let Some(cache) = &mut self.learning {
                cache.record(hiragana, display);
            }
        }
        // 制御文字を除去（学習キャッシュ汚染や予期せぬモデル出力からの防御）。
        let committed: String = committed.chars().filter(|c| !c.is_control()).collect();
        self.commit.text = CString::new(committed).unwrap_or_default();
        self.commit.dirty = true;
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.state = SessionState::Empty;
        self.romaji.reset();
        self.input_buf.clear();
        self.update_preedit("");
    }

    /// 文節変換をキャンセルし、ひらがな preedit を復元して Composing に戻る。
    fn cancel_bunsetsu(&mut self) {
        let hiragana: String = match &self.state {
            SessionState::BunsetsuConversion(conv) => {
                conv.segments.iter().map(|s| s.hiragana.as_str()).collect()
            }
            _ => return,
        };
        self.candidate_cache.items.clear();
        self.candidate_cache.cursor = 0;
        self.live_candidate = None;
        self.state = SessionState::Composing;
        self.input_buf.clear();
        self.input_buf.insert(&hiragana);
        self.romaji.reset();
        self.update_preedit(&hiragana);
    }

    /// ライブ変換でモデル結果よりも優先する候補を返す。
    ///
    /// 学習キャッシュ → ユーザー辞書の順で検索し、最初に見つかった候補を返す。
    /// どちらにもなければ `None`（モデル結果をそのまま使う）。
    fn lookup_live_override(&self, hiragana: &str) -> Option<String> {
        // 1. Learning cache (highest priority)
        if let Some(cache) = &self.learning {
            let results = cache.lookup(hiragana);
            for (surface, _score) in &results {
                let clean: String = surface.chars().filter(|c| !c.is_control()).collect();
                if !clean.is_empty() {
                    return Some(clean);
                }
            }
        }

        // 2. User dictionary
        if let Some(dict) = &self.user_dict {
            if let Some(lr) = dict.exact_match_search(hiragana) {
                if let Some(c) = lr.candidates.first() {
                    let clean: String = c.surface.chars().filter(|c| !c.is_control()).collect();
                    if !clean.is_empty() {
                        return Some(clean);
                    }
                }
            }
        }

        None
    }

    /// Collect conversion candidates: Learning → User Dict → Model → System Dict.
    fn collect_candidates(&self, hiragana: &str) -> Vec<String> {
        // ASCII を含む入力はモデルに渡さない（byte-level BPE デコードで制御文字が生成される）。
        // karukan_convert_top1 と同じ防御策。
        if hiragana.chars().any(|c| c.is_ascii()) {
            return vec![hiragana.to_string()];
        }

        let mut result: Vec<String> = Vec::new();

        // 1. Learning cache (highest priority — user's own history).
        if let Some(cache) = &self.learning {
            for (surface, _score) in cache.lookup(hiragana) {
                // 過去バグで制御文字が記録されている可能性があるためフィルタする。
                let clean: String = surface.chars().filter(|c| !c.is_control()).collect();
                if !clean.is_empty() && !result.contains(&clean) {
                    result.push(clean);
                }
            }
        }

        // 2. User dictionary (higher than model/system dict).
        if let Some(dict) = &self.user_dict {
            if let Some(lr) = dict.exact_match_search(hiragana) {
                for c in lr.candidates {
                    if !result.contains(&c.surface) {
                        result.push(c.surface.clone());
                    }
                }
            }
        }

        // 3. Neural model candidates (beam search, up to 15).
        if let Some(conv) = &self.converter {
            match conv.convert(hiragana, "", 15) {
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

        // 4. System dictionary (fallback).
        if let Some(dict) = &self.dict {
            if let Some(lr) = dict.exact_match_search(hiragana) {
                for c in lr.candidates.iter().take(10) {
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
// Hiragana range check
// ---------------------------------------------------------------------------

/// U+3041–U+3096（ぁ〜ゖ）、U+309D–U+309F（ゝゞ等）、U+30FC（ー）を
/// ひらがなとみなす。句読点・記号・カタカナ等はひらがなではないため false を返す。
pub(crate) fn is_hiragana_char(c: char) -> bool {
    matches!(c, '\u{3041}'..='\u{3096}' | '\u{309D}'..='\u{309F}' | '\u{30FC}')
}

// ---------------------------------------------------------------------------
// 文節分割
// ---------------------------------------------------------------------------

/// ひらがな文字列を助詞境界で文節に分割する。
///
/// 2文字助詞を 1文字助詞より優先してチェックし、助詞をその文節の末尾に含める。
/// 助詞の直後に文字がない場合（末尾助詞）は分割しない。
/// 分割結果がなければ全体を 1 要素で返す。
///
/// # Examples
/// ```
/// // "わたしはがっこうへいきます" → ["わたしは", "がっこうへ", "いきます"]
/// // "せんたく"                   → ["せんたく"]
/// ```
fn segment_hiragana(hiragana: &str) -> Vec<String> {
    const P2: &[&str] = &[
        "から",
        "まで",
        "より",
        "って",
        "けど",
        "ので",
        "のに",
        "には",
        "では",
        "とは",
        "でも",
        "とも",
        "しか",
        "ながら",
    ];
    const P1: &[char] = &[
        'は', 'が', 'を', 'に', 'で', 'へ', 'と', 'も', 'の', 'や', 'か',
    ];

    let chars: Vec<char> = hiragana.chars().collect();
    let n = chars.len();
    if n <= 1 {
        return vec![hiragana.to_string()];
    }

    let mut segments: Vec<String> = Vec::new();
    let mut start: usize = 0;
    let mut i: usize = 1; // 先頭文字は常にセグメントに含める

    while i < n {
        // 2文字助詞チェック（助詞の直後にさらに文字が必要）
        // 「ん」で始まる日本語の単語は存在しないため、分割後の次セグメントが
        // 「ん」始まりになる場合は助詞ではなく語の一部と判断してスキップする。
        if i + 1 < n && i + 2 < n && chars[i + 2] != 'ん' {
            let two: String = chars[i..=i + 1].iter().collect();
            if P2.contains(&two.as_str()) {
                segments.push(chars[start..=i + 1].iter().collect());
                start = i + 2;
                i = start + 1;
                continue;
            }
        }
        // 1文字助詞チェック（助詞の直後にさらに文字が必要）
        if i + 1 < n && P1.contains(&chars[i]) && chars[i + 1] != 'ん' {
            segments.push(chars[start..=i].iter().collect());
            start = i + 1;
            i = start + 1;
            continue;
        }
        i += 1;
    }

    // 残り
    if start < n {
        segments.push(chars[start..].iter().collect());
    }

    if segments.is_empty() {
        segments.push(hiragana.to_string());
    }
    segments
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
        ("きゃ", "kya"),
        ("きゅ", "kyu"),
        ("きょ", "kyo"),
        ("きぃ", "kyi"),
        ("きぇ", "kye"),
        // くぁ行
        ("くぁ", "kwa"),
        ("くぃ", "kwi"),
        ("くぅ", "kwu"),
        ("くぇ", "kwe"),
        ("くぉ", "kwo"),
        // ぎゃ行
        ("ぎゃ", "gya"),
        ("ぎゅ", "gyu"),
        ("ぎょ", "gyo"),
        ("ぎぃ", "gyi"),
        ("ぎぇ", "gye"),
        // ぐぁ行
        ("ぐぁ", "gwa"),
        ("ぐぃ", "gwi"),
        ("ぐぅ", "gwu"),
        ("ぐぇ", "gwe"),
        ("ぐぉ", "gwo"),
        // しゃ行
        ("しゃ", "sha"),
        ("しゅ", "shu"),
        ("しょ", "sho"),
        ("しぃ", "syi"),
        ("しぇ", "she"),
        // すぁ行
        ("すぁ", "swa"),
        ("すぃ", "swi"),
        ("すぅ", "swu"),
        ("すぇ", "swe"),
        ("すぉ", "swo"),
        // じゃ行
        ("じゃ", "ja"),
        ("じゅ", "ju"),
        ("じょ", "jo"),
        ("じぃ", "zyi"),
        ("じぇ", "je"),
        // ずぁ行
        ("ずぁ", "zwa"),
        ("ずぃ", "zwi"),
        ("ずぅ", "zwu"),
        ("ずぇ", "zwe"),
        ("ずぉ", "zwo"),
        // ちゃ行
        ("ちゃ", "cha"),
        ("ちゅ", "chu"),
        ("ちょ", "cho"),
        ("ちぃ", "tyi"),
        ("ちぇ", "che"),
        // つぁ行
        ("つぁ", "tsa"),
        ("つぃ", "tsi"),
        ("つぇ", "tse"),
        ("つぉ", "tso"),
        // てゃ行
        ("てゃ", "tha"),
        ("てぃ", "thi"),
        ("てゅ", "thu"),
        ("てぇ", "the"),
        ("てょ", "tho"),
        // とぁ行
        ("とぁ", "twa"),
        ("とぃ", "twi"),
        ("とぅ", "twu"),
        ("とぇ", "twe"),
        ("とぉ", "two"),
        // ぢゃ行
        ("ぢゃ", "dya"),
        ("ぢゅ", "dyu"),
        ("ぢょ", "dyo"),
        ("ぢぃ", "dyi"),
        ("ぢぇ", "dye"),
        // でゃ行
        ("でゃ", "dha"),
        ("でぃ", "dhi"),
        ("でゅ", "dhu"),
        ("でぇ", "dhe"),
        ("でょ", "dho"),
        // どぁ行
        ("どぁ", "dwa"),
        ("どぃ", "dwi"),
        ("どぅ", "dwu"),
        ("どぇ", "dwe"),
        ("どぉ", "dwo"),
        // にゃ行
        ("にゃ", "nya"),
        ("にゅ", "nyu"),
        ("にょ", "nyo"),
        ("にぃ", "nyi"),
        ("にぇ", "nye"),
        // ひゃ行
        ("ひゃ", "hya"),
        ("ひゅ", "hyu"),
        ("ひょ", "hyo"),
        ("ひぃ", "hyi"),
        ("ひぇ", "hye"),
        // ふぁ行
        ("ふぁ", "fa"),
        ("ふぃ", "fi"),
        ("ふぇ", "fe"),
        ("ふぉ", "fo"),
        ("ふゃ", "fya"),
        ("ふゅ", "fyu"),
        ("ふょ", "fyo"),
        // びゃ行
        ("びゃ", "bya"),
        ("びゅ", "byu"),
        ("びょ", "byo"),
        ("びぃ", "byi"),
        ("びぇ", "bye"),
        // ぴゃ行
        ("ぴゃ", "pya"),
        ("ぴゅ", "pyu"),
        ("ぴょ", "pyo"),
        ("ぴぃ", "pyi"),
        ("ぴぇ", "pye"),
        // みゃ行
        ("みゃ", "mya"),
        ("みゅ", "myu"),
        ("みょ", "myo"),
        ("みぃ", "myi"),
        ("みぇ", "mye"),
        // りゃ行
        ("りゃ", "rya"),
        ("りゅ", "ryu"),
        ("りょ", "ryo"),
        ("りぃ", "ryi"),
        ("りぇ", "rye"),
        // うぁ行
        ("うぁ", "wha"),
        ("うぃ", "wi"),
        ("うぇ", "we"),
        ("うぉ", "who"),
        // いぇ
        ("いぇ", "ye"),
        // ゔ行
        ("ゔぁ", "va"),
        ("ゔぃ", "vi"),
        ("ゔぇ", "ve"),
        ("ゔぉ", "vo"),
        ("ゔゃ", "vya"),
        ("ゔゅ", "vyu"),
        ("ゔょ", "vyo"),
        // ── 単独かな ──
        ("あ", "a"),
        ("い", "i"),
        ("う", "u"),
        ("え", "e"),
        ("お", "o"),
        ("か", "ka"),
        ("き", "ki"),
        ("く", "ku"),
        ("け", "ke"),
        ("こ", "ko"),
        ("さ", "sa"),
        ("し", "shi"),
        ("す", "su"),
        ("せ", "se"),
        ("そ", "so"),
        ("た", "ta"),
        ("ち", "chi"),
        ("つ", "tsu"),
        ("て", "te"),
        ("と", "to"),
        ("な", "na"),
        ("に", "ni"),
        ("ぬ", "nu"),
        ("ね", "ne"),
        ("の", "no"),
        ("は", "ha"),
        ("ひ", "hi"),
        ("ふ", "fu"),
        ("へ", "he"),
        ("ほ", "ho"),
        ("ま", "ma"),
        ("み", "mi"),
        ("む", "mu"),
        ("め", "me"),
        ("も", "mo"),
        ("や", "ya"),
        ("ゆ", "yu"),
        ("よ", "yo"),
        ("ら", "ra"),
        ("り", "ri"),
        ("る", "ru"),
        ("れ", "re"),
        ("ろ", "ro"),
        ("わ", "wa"),
        ("を", "wo"),
        ("ん", "nn"),
        // 濁音
        ("が", "ga"),
        ("ぎ", "gi"),
        ("ぐ", "gu"),
        ("げ", "ge"),
        ("ご", "go"),
        ("ざ", "za"),
        ("じ", "ji"),
        ("ず", "zu"),
        ("ぜ", "ze"),
        ("ぞ", "zo"),
        ("だ", "da"),
        ("ぢ", "di"),
        ("づ", "du"),
        ("で", "de"),
        ("ど", "do"),
        ("ば", "ba"),
        ("び", "bi"),
        ("ぶ", "bu"),
        ("べ", "be"),
        ("ぼ", "bo"),
        // 半濁音
        ("ぱ", "pa"),
        ("ぴ", "pi"),
        ("ぷ", "pu"),
        ("ぺ", "pe"),
        ("ぽ", "po"),
        // ゔ
        ("ゔ", "vu"),
        // 小文字
        ("ぁ", "xa"),
        ("ぃ", "xi"),
        ("ぅ", "xu"),
        ("ぇ", "xe"),
        ("ぉ", "xo"),
        ("ゃ", "xya"),
        ("ゅ", "xyu"),
        ("ょ", "xyo"),
        ("っ", "xtu"),
        ("ゎ", "xwa"),
        // 歴史的かな
        ("ゐ", "wyi"),
        ("ゑ", "wye"),
        // 長音記号
        ("ー", "-"),
        // 句読点・記号
        ("、", ","),
        ("。", "."),
        ("・", "/"),
        ("？", "?"),
        ("！", "!"),
        ("〜", "~"),
        ("「", "["),
        ("」", "]"),
        ("『", "z["),
        ("』", "z]"),
        ("…", "z."),
        ("‥", "z,"),
        ("←", "zh"),
        ("↓", "zj"),
        ("↑", "zk"),
        ("→", "zl"),
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
        // 1 回目の Space: BunsetsuConversion（候補は lazy loading で未ロード）
        s.push_key(KarukanKey::Space);
        assert!(!s.is_empty());
        assert!(
            s.candidate_cache.items.is_empty(),
            "candidates not yet loaded after first Space (lazy loading)"
        );
        // 2 回目の Space: show_segment_candidates で候補ロード
        s.push_key(KarukanKey::Space);
        assert!(
            !s.candidate_cache.items.is_empty(),
            "candidates loaded on second Space"
        );
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
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        // 1 回目の Space: BunsetsuConversion（候補は lazy loading で未ロード）
        s.push_key(KarukanKey::Space);
        assert!(
            s.candidate_cache.items.is_empty(),
            "candidates not yet loaded after first Space (lazy loading)"
        );
        // 2 回目の Space: show_segment_candidates で候補ロード
        s.push_key(KarukanKey::Space);
        assert!(
            !s.candidate_cache.items.is_empty(),
            "candidates loaded on second Space"
        );
    }

    #[test]
    fn test_select_candidate() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        // 1 回目の Space: BunsetsuConversion（候補 lazy）
        s.push_key(KarukanKey::Space);
        // 2 回目の Space: show_segment_candidates で候補ロード
        s.push_key(KarukanKey::Space);
        let ok = s.select_candidate(0);
        assert!(ok);
        // select_candidate は選択文節の display を更新し、全文節を即コミットする。
        assert!(
            s.commit.dirty,
            "select_candidate should commit all segments"
        );
        assert!(s.is_empty(), "should be Empty after committing");
    }

    #[test]
    fn test_space_on_empty_inserts_fullwidth_space() {
        let mut s = KarukanSession::new();
        // Space when Empty → 全角スペース (U+3000) をコミットして consumed。
        let consumed = s.push_key(KarukanKey::Space);
        assert!(consumed, "Space in Empty should be consumed");
        assert!(s.commit.dirty, "commit should be dirty");
        assert_eq!(s.commit.text.to_str().unwrap(), "\u{3000}");
        assert!(s.is_empty(), "state should remain Empty");
    }

    #[test]
    fn test_space_on_composing_triggers_conversion_not_fullwidth() {
        let mut s = KarukanSession::new();
        s.push_char('a'); // "あ" in composing
        s.push_key(KarukanKey::Space);
        // Composing + Space → Conversion。全角スペースはコミットしない。
        assert!(
            !s.commit.dirty,
            "should not commit fullwidth space in composing mode"
        );
    }

    // ── Convert shortcut tests ──

    #[test]
    fn test_convert_hiragana_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        assert_eq!(s.preedit.text.to_str().unwrap(), "にほんご");
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "にほんご");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_katakana_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "ニホンゴ");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_ascii_from_composing() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.push_key(KarukanKey::ConvertAscii);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "nihonngo");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_hiragana_from_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space); // enter BunsetsuConversion（候補 lazy）
        // BunsetsuConversion 中の Ctrl+J → 選択文節の display をひらがなに変更（確定しない）
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(!s.commit.dirty);
        assert!(matches!(s.state, SessionState::BunsetsuConversion(_)));
        assert_eq!(s.preedit.text.to_str().unwrap(), "あ");
        assert!(s.candidate_cache.items.is_empty());
    }

    #[test]
    fn test_convert_katakana_from_conversion() {
        let mut s = KarukanSession::new();
        s.push_char('a');
        s.push_key(KarukanKey::Space);
        // BunsetsuConversion 中の Ctrl+K → 選択文節の display をカタカナに変更（確定しない）
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(!s.commit.dirty);
        assert!(matches!(s.state, SessionState::BunsetsuConversion(_)));
        assert_eq!(s.preedit.text.to_str().unwrap(), "ア");
    }

    #[test]
    fn test_convert_ascii_from_conversion() {
        let mut s = KarukanSession::new();
        "ka".chars().for_each(|c| {
            s.push_char(c);
        });
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

    // ── Live conversion + Ctrl+J/K two-step tests ──

    #[test]
    fn test_convert_hiragana_cancels_live_first() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        // Simulate live conversion
        s.live_candidate = Some("日本語".to_string());
        s.live_candidate_source = s.input_buf.text.clone();

        // 1st Ctrl+J: cancel live, show hiragana preedit (no commit)
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(!s.commit.dirty, "1st Ctrl+J should not commit");
        assert!(
            s.live_candidate.is_none(),
            "live_candidate should be cleared"
        );
        assert_eq!(s.preedit.text.to_str().unwrap(), "にほんご");
        assert!(!s.is_empty(), "should remain in Composing");

        // 2nd Ctrl+J: commit hiragana
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty, "2nd Ctrl+J should commit");
        assert_eq!(s.commit.text.to_str().unwrap(), "にほんご");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_katakana_cancels_live_first() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.live_candidate = Some("日本語".to_string());
        s.live_candidate_source = s.input_buf.text.clone();

        // 1st Ctrl+K: cancel live, show katakana preedit (no commit)
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(!s.commit.dirty, "1st Ctrl+K should not commit");
        assert!(s.live_candidate.is_none());
        assert_eq!(s.preedit.text.to_str().unwrap(), "ニホンゴ");
        assert!(!s.is_empty(), "should remain in Composing");

        // 2nd Ctrl+K: commit katakana
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(s.commit.dirty, "2nd Ctrl+K should commit");
        assert_eq!(s.commit.text.to_str().unwrap(), "ニホンゴ");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_hiragana_no_live_commits_immediately() {
        // Without live conversion, Ctrl+J should commit immediately (no change)
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        assert!(s.live_candidate.is_none());
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "にほんご");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_mode_switch_j_then_k() {
        // 日本語 (live) → Ctrl+J → にほんご (preedit) → Ctrl+K → ニホンゴ (preedit, NOT commit)
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.live_candidate = Some("日本語".to_string());
        s.live_candidate_source = s.input_buf.text.clone();

        // Ctrl+J: cancel live, show hiragana preedit
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(!s.commit.dirty);
        assert_eq!(s.preedit.text.to_str().unwrap(), "にほんご");

        // Ctrl+K: switch to katakana preedit (should NOT commit)
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(!s.commit.dirty, "mode switch should not commit");
        assert_eq!(s.preedit.text.to_str().unwrap(), "ニホンゴ");
        assert!(!s.is_empty());

        // Ctrl+K again: now commit katakana
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "ニホンゴ");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_mode_switch_k_then_j() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.live_candidate = Some("日本語".to_string());
        s.live_candidate_source = s.input_buf.text.clone();

        // Ctrl+K → Ctrl+J → Ctrl+J で確定
        s.push_key(KarukanKey::ConvertKatakana);
        assert!(!s.commit.dirty);
        assert_eq!(s.preedit.text.to_str().unwrap(), "ニホンゴ");

        s.push_key(KarukanKey::ConvertHiragana);
        assert!(!s.commit.dirty);
        assert_eq!(s.preedit.text.to_str().unwrap(), "にほんご");

        s.push_key(KarukanKey::ConvertHiragana);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "にほんご");
        assert!(s.is_empty());
    }

    #[test]
    fn test_convert_preview_cleared_by_char_input() {
        let mut s = KarukanSession::new();
        "nihongo".chars().for_each(|c| {
            s.push_char(c);
        });
        s.live_candidate = Some("日本語".to_string());
        s.live_candidate_source = s.input_buf.text.clone();

        // Ctrl+J: enter preview mode
        s.push_key(KarukanKey::ConvertHiragana);
        assert!(!s.commit.dirty);
        assert!(s.convert_preview.is_some());

        // Type a char: preview should be cleared, back to normal composing
        s.push_char('g');
        assert!(s.convert_preview.is_none());
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
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
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
        "nadesi".chars().for_each(|c| {
            s.push_char(c);
        });
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
        "nadesi".chars().for_each(|c| {
            s.push_char(c);
        });

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

        // romaji バッファに未確定子音がある場合は apply_live_candidate を無視する。
        // auto-commit が pending 子音を失うのを防ぐため。
        s.apply_live_candidate("亜", "あ");
        // preedit は push_char が設定した "あk" のまま維持される
        assert_eq!(s.preedit.text.to_str().unwrap(), "あk");
    }

    #[test]
    fn test_backspace_clears_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
        s.apply_live_candidate("愛憂", "あいう");
        assert_eq!(s.preedit.text.to_str().unwrap(), "愛憂");

        s.push_key(KarukanKey::Backspace);
        // live_candidate がクリアされ、ひらがな表示 ("あい") に戻る
        assert_eq!(s.preedit.text.to_str().unwrap(), "あい");
    }

    #[test]
    fn test_commit_uses_live_candidate_when_source_matches() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
        s.apply_live_candidate("愛憂", "あいう");

        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        assert_eq!(s.commit.text.to_str().unwrap(), "愛憂");
        assert!(s.is_empty());
    }

    #[test]
    fn test_commit_live_candidate_with_pending_consonant() {
        // 再現: ライブ変換適用済み → 子音 pending → Return
        // 期待: ライブ変換結果 + pending 子音のひらがな化、ASCII 混在しないこと
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
        // ライブ変換適用（source 一致）
        s.apply_live_candidate("愛憂", "あいう");
        assert_eq!(s.live_candidate.as_deref(), Some("愛憂"));

        // 子音 'k' を追加（romaji buffer に pending）
        s.push_char('k');
        assert!(
            !s.romaji.buffer().is_empty(),
            "romaji buffer should have 'k'"
        );

        // Return でコミット → ASCII 'k' が混入しないことを確認
        s.push_key(KarukanKey::Return);
        assert!(s.commit.dirty);
        let committed = s.commit.text.to_str().unwrap();
        assert!(
            !committed.contains('k'),
            "committed text should not contain ASCII 'k', got: '{}'",
            committed
        );
    }

    #[test]
    fn test_commit_ignores_stale_live_candidate() {
        let mut s = KarukanSession::new();
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
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
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
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
        "aiu".chars().for_each(|c| {
            s.push_char(c);
        });
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

    // ── User dictionary tests ──

    /// Helper: create a temp dir with user_dicts/ containing a TSV file,
    /// and return a session with `data_dir` pointing to it.
    fn setup_user_dict_session(entries: &[(&str, &str)]) -> (KarukanSession, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let user_dicts = tmp.path().join("user_dicts");
        std::fs::create_dir_all(&user_dicts).unwrap();

        let mut content = String::from("# test user dictionary\n");
        for (reading, surface) in entries {
            content.push_str(&format!("{}\t{}\n", reading, surface));
        }
        std::fs::write(user_dicts.join("test.tsv"), &content).unwrap();

        let mut s = KarukanSession::new();
        s.data_dir = Some(tmp.path().to_path_buf());
        (s, tmp)
    }

    #[test]
    fn test_user_dict_loaded() {
        let (mut s, _tmp) =
            setup_user_dict_session(&[("かるかん", "Karukan"), ("てすと", "TestWord")]);
        s.init_resources();

        assert!(s.user_dict.is_some(), "user_dict should be loaded");
    }

    #[test]
    fn test_user_dict_candidates_appear() {
        let (mut s, _tmp) = setup_user_dict_session(&[("かるかん", "Karukan")]);
        s.init_resources();

        let candidates = s.collect_candidates("かるかん");
        assert!(
            candidates.contains(&"Karukan".to_string()),
            "user dict entry should appear in candidates: {:?}",
            candidates
        );
    }

    #[test]
    fn test_user_dict_priority_over_system_dict() {
        let (mut s, _tmp) = setup_user_dict_session(&[("あ", "UserA")]);
        s.init_resources();

        let candidates = s.collect_candidates("あ");
        // User dict entry should come before hiragana fallback
        let user_pos = candidates.iter().position(|c| c == "UserA");
        let fallback_pos = candidates.iter().position(|c| c == "あ");
        assert!(
            user_pos.is_some(),
            "UserA should be in candidates: {:?}",
            candidates
        );
        if let (Some(u), Some(f)) = (user_pos, fallback_pos) {
            assert!(u < f, "user dict should come before fallback");
        }
    }

    #[test]
    fn test_user_dict_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dicts = tmp.path().join("user_dicts");
        std::fs::create_dir_all(&user_dicts).unwrap();
        // No files in user_dicts/

        let mut s = KarukanSession::new();
        s.data_dir = Some(tmp.path().to_path_buf());
        s.init_resources();
        assert!(s.user_dict.is_none(), "no files → user_dict should be None");
    }

    #[test]
    fn test_user_dict_no_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Don't create user_dicts/ at all

        let mut s = KarukanSession::new();
        s.data_dir = Some(tmp.path().to_path_buf());
        s.init_resources();
        assert!(
            s.user_dict.is_none(),
            "missing dir → user_dict should be None"
        );
    }

    // ── Live conversion + user dictionary tests ──

    #[test]
    fn test_live_conversion_uses_user_dict() {
        let (mut s, _tmp) = setup_user_dict_session(&[("かるかん", "Karukan")]);
        s.init_resources();

        // Simulate typing "かるかん" → Composing state
        s.input_buf.insert("かるかん");
        s.state = SessionState::Composing;
        s.update_preedit("かるかん");

        // Model returns something generic, but user dict should override
        s.apply_live_candidate("軽羹", "かるかん");

        assert_eq!(
            s.live_candidate.as_deref(),
            Some("Karukan"),
            "user dict should override model result in live conversion"
        );
    }

    #[test]
    fn test_live_conversion_model_result_when_no_user_dict_match() {
        let (mut s, _tmp) = setup_user_dict_session(&[("かるかん", "Karukan")]);
        s.init_resources();

        // Simulate typing "にほんご"
        s.input_buf.insert("にほんご");
        s.state = SessionState::Composing;
        s.update_preedit("にほんご");

        // No user dict match for "にほんご" → model result used as-is
        s.apply_live_candidate("日本語", "にほんご");

        assert_eq!(
            s.live_candidate.as_deref(),
            Some("日本語"),
            "model result should be used when no user dict match"
        );
    }

    #[test]
    fn test_live_conversion_learning_overrides_user_dict() {
        let (mut s, _tmp) = setup_user_dict_session(&[("かるかん", "Karukan")]);
        s.init_resources();

        // Record a learning entry that should take priority over user dict
        if let Some(cache) = &mut s.learning {
            cache.record("かるかん", "軽羹");
        }

        s.input_buf.insert("かるかん");
        s.state = SessionState::Composing;
        s.update_preedit("かるかん");

        s.apply_live_candidate("something", "かるかん");

        assert_eq!(
            s.live_candidate.as_deref(),
            Some("軽羹"),
            "learning cache should override user dict in live conversion"
        );
    }

    // -----------------------------------------------------------------------
    // segment_hiragana tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_segment_single_char() {
        assert_eq!(segment_hiragana("あ"), vec!["あ"]);
    }

    #[test]
    fn test_segment_no_particle() {
        assert_eq!(segment_hiragana("きゅうでん"), vec!["きゅうでん"]);
    }

    #[test]
    fn test_segment_particle_de_before_n() {
        // 「で」の直後が「ん」の場合は助詞ではなく語の一部（例: 給電、本殿）
        assert_eq!(segment_hiragana("ほんでん"), vec!["ほんでん"]);
    }

    #[test]
    fn test_segment_particle_ha() {
        assert_eq!(
            segment_hiragana("きょうはいいてんき"),
            vec!["きょうは", "いいてんき"]
        );
    }

    #[test]
    fn test_segment_particle_wo() {
        assert_eq!(
            segment_hiragana("ほんをよむ"),
            vec!["ほんを", "よむ"]
        );
    }

    #[test]
    fn test_segment_particle_at_end_no_split() {
        // 末尾が助詞のみの場合は分割しない
        assert_eq!(segment_hiragana("わたしは"), vec!["わたしは"]);
    }

    #[test]
    fn test_segment_particle_ni_before_n() {
        // 「に」の直後が「ん」→ 助詞ではなく語の一部（例: にんじん の途中）
        assert_eq!(segment_hiragana("かにんべん"), vec!["かにんべん"]);
    }

    #[test]
    fn test_segment_two_char_particle() {
        assert_eq!(
            segment_hiragana("えきからあるく"),
            vec!["えきから", "あるく"]
        );
    }

    #[test]
    fn test_segment_multiple_particles() {
        assert_eq!(
            segment_hiragana("わたしはきょうでかける"),
            vec!["わたしは", "きょうで", "かける"]
        );
    }
}
