# Phase 1 実装計画書: Rust FFI クレート (`karukan-macos`)

> ブランチ: `feature/macos-phase1-ffi`
> 前提: `epic/macos` から切る
> 完了条件: `cargo test -p karukan-macos` が全パス

---

## 目標

`karukan-engine` をmacOS向けにラップした `cdylib` を作成する。
**Phase 1 スコープはローマ字→ひらがな変換のみ**。漢字変換・モデルロードは Phase 3 で追加する。

---

## 依存関係の設計判断

### `karukan-im` には依存しない

`karukan-im` は `InputMethodEngine` という高機能な状態機械を持つが、以下の理由で `karukan-macos` の依存には使わない。

| 問題 | 内容 |
|---|---|
| `Settings::data_dir()` / `Settings::learning_file()` | XDGパスをハードコードしている（macOSで `None` になる） |
| `Settings::learning_file()` が `save_learning` 内部で呼ばれる | macOS向けパスを外部から渡す手段がない |
| `karukan-im` クレートの責務 | Linux/fcitx5専用と明示されており変更禁止 |

### `karukan-engine` に直接依存する

`karukan-macos` は `karukan-engine` の公開APIのみを使用し、独自の `KarukanSession` 状態機械を実装する。これは `karukan-im/src/core/engine/` が `karukan-engine` をラップしているのと同じ構造。

```text
karukan-engine  (変更禁止)
    ↑
karukan-macos   (新規 cdylib)   ←── Phase 1 で作成
    ↑
KarukanIM.xcodeproj (Phase 2 以降)
```

---

## ファイル構成と各ファイルの責務

```text
karukan-macos/
├── Cargo.toml
└── src/
    ├── lib.rs              # クレートルート。モジュール宣言のみ
    ├── session.rs          # KarukanSession 型（非公開の状態機械）
    ├── platform/
    │   ├── mod.rs
    │   └── paths.rs        # macOS データディレクトリ解決
    └── ffi/
        ├── mod.rs          # ffi_ref!/ffi_mut! マクロ、ログ初期化
        ├── lifecycle.rs    # session_new / session_init / session_free
        ├── input.rs        # push_char / push_key
        ├── query.rs        # get_preedit / get_commit / is_empty 等
        └── tests.rs        # FFI境界テスト
```

---

## `Cargo.toml`

```toml
[package]
name = "karukan-macos"
version = "0.1.0"
edition.workspace = true
description = "Japanese Input Method Engine for macOS"
license.workspace = true

[lib]
name = "karukan_macos"
crate-type = ["cdylib", "lib"]   # cdylib: Xcode組み込み用, lib: テスト用

[dependencies]
karukan-engine.workspace = true
anyhow.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
dirs = "5"

[dev-dependencies]
tempfile.workspace = true
```

> **`crate-type` の両方が必要な理由**: `cdylib` のみだと `cargo test` が動かない。`lib` も指定することで Rust 単体テストが実行できる。

---

## `session.rs` — 状態機械

### 設計方針

Phase 1 では `Empty` / `Composing` の 2 状態のみ。
`Conversion` 状態（候補選択）は Phase 3 で追加する。

```text
karukan_push_char(印字可能文字)
        ↓
    Empty ──────────────→ Composing
                          （ひらがなバッファに蓄積）

karukan_push_key(RETURN/ESCAPE)
        ↓
    Composing ──────────→ Empty
                          （RETURN: commit生成 / ESCAPE: 破棄）

karukan_push_key(BACKSPACE)
        ↓
    Composing ──────── バッファが空になれば → Empty
                       （それ以外は Composing 継続）
```

### 構造体

```rust
// session.rs

use std::ffi::CString;
use karukan_engine::{BackspaceResult, ConversionEvent, RomajiConverter};

/// IME 入力バッファ（ひらがなテキスト + カーソル位置）
struct InputBuffer {
    /// 確定ひらがな列（UTF-8）
    text: String,
    /// カーソル位置（文字数オフセット）
    cursor_chars: usize,
}

impl InputBuffer {
    fn new() -> Self { Self { text: String::new(), cursor_chars: 0 } }
    fn is_empty(&self) -> bool { self.text.is_empty() }
    fn clear(&mut self) { self.text.clear(); self.cursor_chars = 0; }

    /// カーソル位置に文字列を挿入
    fn insert(&mut self, s: &str) {
        let byte_pos = self.text
            .char_indices()
            .nth(self.cursor_chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len());
        self.text.insert_str(byte_pos, s);
        self.cursor_chars += s.chars().count();
    }

    /// カーソル直前の文字を削除。削除した文字を返す
    fn delete_before_cursor(&mut self) -> Option<char> {
        if self.cursor_chars == 0 { return None; }
        let char_pos = self.cursor_chars - 1;
        let byte_pos = self.text
            .char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)?;
        let ch = self.text.remove(byte_pos);
        self.cursor_chars -= 1;
        Some(ch)
    }

    /// カーソルバイトオフセット（FFI返却用）
    fn cursor_byte_offset(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor_chars)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

/// IME セッション状態
enum SessionState {
    Empty,
    Composing,
    // Conversion は Phase 3 で追加
}

/// preedit キャッシュ（FFI 返却用 CString を保持）
#[derive(Default)]
pub(crate) struct PreeditCache {
    pub text: CString,
    pub caret_bytes: u32,
    pub dirty: bool,
}

/// commit キャッシュ
#[derive(Default)]
pub(crate) struct CommitCache {
    pub text: CString,
    pub dirty: bool,
}

/// Phase 1 の KarukanSession（漢字変換なし）
pub struct KarukanSession {
    state: SessionState,
    romaji: RomajiConverter,
    input_buf: InputBuffer,
    pub(crate) preedit: PreeditCache,
    pub(crate) commit: CommitCache,
}
```

### 主要メソッド（実装指針）

```rust
impl KarukanSession {
    /// 軽量生成（モデルロードなし）
    pub fn new() -> Self { ... }

    /// push_char の内部処理
    /// 戻り値: IMEが消費したか否か
    pub fn push_char(&mut self, ch: char) -> bool {
        self.clear_flags();

        // RomajiConverter に文字を渡す
        let prev_output_len = self.romaji.output().chars().count();
        let event = self.romaji.push(ch);

        // output の差分をひらがなバッファに追加
        let new_hiragana: String = self.romaji.output()
            .chars()
            .skip(prev_output_len)
            .collect();
        if !new_hiragana.is_empty() {
            self.input_buf.insert(&new_hiragana);
        }

        // ローマ字バッファ（未確定部分）を preedit に含める
        let romaji_buf = self.romaji.buffer().to_string();
        let preedit_text = format!("{}{}", self.input_buf.text, romaji_buf);

        if preedit_text.is_empty() {
            self.state = SessionState::Empty;
            self.update_preedit("");
        } else {
            self.state = SessionState::Composing;
            self.update_preedit(&preedit_text);
        }

        match event {
            // ローマ字変換できないケース（"x" 単独等）
            // 変換途中も含め、IMEとして消費したとみなす
            _ => true,
        }
    }

    /// push_key の内部処理
    pub fn push_key(&mut self, key: KarukanKey) -> bool {
        self.clear_flags();
        match (&self.state, key) {
            (SessionState::Empty, _) => false,  // Empty時は全てスルー
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
            // Phase 3 で Space に変換トリガーを追加
            (SessionState::Composing, KarukanKey::Space) => {
                // Phase 1: スペースをひらがなとして確定
                self.input_buf.insert("　");  // 全角スペース
                self.update_preedit(&self.input_buf.text.clone());
                true
            }
            _ => false,
        }
    }

    /// RomajiConverter + InputBuffer をリセットしてコミットを生成
    fn do_commit(&mut self) {
        // ローマ字バッファを flush してひらがなに
        let prev_len = self.romaji.output().chars().count();
        self.romaji.flush();
        let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
        if !flushed.is_empty() {
            self.input_buf.insert(&flushed);
        }

        let committed = self.input_buf.text.clone();
        self.romaji.reset();
        self.input_buf.clear();
        self.state = SessionState::Empty;

        self.commit.text = CString::new(committed).unwrap_or_default();
        self.commit.dirty = true;
        self.update_preedit("");
    }

    fn do_cancel(&mut self) {
        self.romaji.reset();
        self.input_buf.clear();
        self.state = SessionState::Empty;
        self.update_preedit("");
    }

    fn do_backspace(&mut self) {
        // BackspaceResult の各 variant の意味:
        //   RemovedBuffer(ch) … 未確定ローマ字バッファから ch を削除した
        //   RemovedOutput(ch) … 確定済み output から ch を削除した
        //   Empty             … バッファも output も空だった
        //
        // 重要: romaji.output() は push_char() で input_buf に delta 追記している。
        // RemovedOutput が返ってきた場合、romaji.output から文字が消えただけでなく
        // input_buf にも同じ文字が insert 済みのため、input_buf からも削除する。
        let result = self.romaji.backspace();
        match result {
            BackspaceResult::RemovedBuffer(_) => {
                // ローマ字バッファ内で完結。input_buf は変更不要
            }
            BackspaceResult::RemovedOutput(_) => {
                // romaji.output の末尾が削除された = input_buf にも insert 済み
                // → input_buf からも削除して同期を取る
                self.input_buf.delete_before_cursor();
            }
            BackspaceResult::Empty => {
                // romaji も input_buf も空。何もしない
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

    fn update_preedit(&mut self, text: &str) {
        let caret = self.input_buf.cursor_byte_offset()
            + self.romaji.buffer().len();  // ローマ字分もキャレット位置に加算
        self.preedit.text = CString::new(text).unwrap_or_default();
        self.preedit.caret_bytes = caret as u32;
        self.preedit.dirty = true;
    }

    fn clear_flags(&mut self) {
        self.preedit.dirty = false;
        self.commit.dirty = false;
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.state, SessionState::Empty)
    }
}
```

> **`RomajiConverter` の `backspace()` API確認が必要**: `karukan-engine` の `BackspaceResult` に
> `Consumed` / `NotConsumed` が存在することは `karukan-im` のコードから確認済み。
> Phase 1 実装前に最新の公開APIを `cargo doc -p karukan-engine --open` で確認すること。

---

## `platform/paths.rs`

```rust
use std::path::PathBuf;

/// アプリケーションデータディレクトリを返す。
///
/// 本番（IMEプロセス）: ~/Library/Application Support/Karukan/
/// テスト・CI:         $KARUKAN_DATA_DIR（環境変数）
pub fn app_support_dir() -> PathBuf {
    if let Ok(p) = std::env::var("KARUKAN_DATA_DIR") {
        return PathBuf::from(p);
    }
    // dirs::data_local_dir() は macOS で ~/Library/Application Support/ を返す
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
        .join("Karukan")
}

pub fn models_dir() -> PathBuf {
    app_support_dir().join("models")
}

pub fn learning_cache_path() -> PathBuf {
    app_support_dir().join("learning.tsv")
}

pub fn user_dict_dir() -> PathBuf {
    app_support_dir().join("user_dicts")
}

pub fn system_dict_path() -> PathBuf {
    app_support_dir().join("dict.bin")
}
```

---

## `ffi/mod.rs` — 共通マクロとログ初期化

```rust
use std::sync::Once;

/// null チェック + 共有参照取得（戻り値あり関数用）
macro_rules! ffi_ref {
    ($ptr:expr, $default:expr) => {{
        if $ptr.is_null() { return $default; }
        unsafe { &*$ptr }
    }};
}

/// null チェック + 可変参照取得
macro_rules! ffi_mut {
    ($ptr:expr) => {{
        if $ptr.is_null() { return; }
        unsafe { &mut *$ptr }
    }};
    ($ptr:expr, $default:expr) => {{
        if $ptr.is_null() { return $default; }
        unsafe { &mut *$ptr }
    }};
}

pub(crate) use ffi_ref;
pub(crate) use ffi_mut;

static INIT_LOGGING: Once = Once::new();

pub(crate) fn init_logging() {
    INIT_LOGGING.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_writer(std::io::stderr)
            .init();
    });
}
```

---

## `ffi/lifecycle.rs` — セッション生成・初期化・解放

### `karukan_session_new`

```rust
/// セッションを生成する（軽量。メインスレッドで安全に呼べる）
/// 戻り値: セッションポインタ。失敗時は null
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_new() -> *mut KarukanSession {
    std::panic::catch_unwind(|| {
        init_logging();
        Box::into_raw(Box::new(KarukanSession::new()))
    })
    .unwrap_or(std::ptr::null_mut())
}
```

### `karukan_session_init`

```rust
/// セッションを初期化する（辞書・学習キャッシュのロード）
///
/// Phase 1 では辞書・学習キャッシュのみ。モデルロードは Phase 3 で追加。
/// ファイルが存在しない場合は非致命的スキップ（-1 は返さない）。
///
/// # 呼び出し規約
/// Swift 側はバックグラウンドスレッドから呼ぶこと（Phase 3でモデルロードが追加されるため）。
/// 戻り値: 0=成功, -1=失敗（現時点では常に 0）
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_init(session: *mut KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        let session = ffi_mut!(session, -1);
        session.init_resources();
        0
    })
    .unwrap_or(-1)
}
```

**`KarukanSession::init_resources()` の内容（Phase 1）**:

```rust
impl KarukanSession {
    pub fn init_resources(&mut self) {
        // 辞書ロード（ファイルなければスキップ）
        let dict_path = crate::platform::paths::system_dict_path();
        if dict_path.exists() {
            match karukan_engine::Dictionary::load(&dict_path) {
                Ok(d) => { self.dict = Some(d); }
                Err(e) => tracing::warn!("dict load failed: {}", e),
            }
        }

        // 学習キャッシュロード（ファイルなければ空で初期化）
        let learning_path = crate::platform::paths::learning_cache_path();
        self.learning = Some(if learning_path.exists() {
            karukan_engine::LearningCache::load(&learning_path, 10_000)
                .unwrap_or_else(|_| karukan_engine::LearningCache::new(10_000))
        } else {
            karukan_engine::LearningCache::new(10_000)
        });

        // Phase 3 で追加: KanaKanjiConverter のロード（バックグラウンドスレッド必須）
    }
}
```

> **Phase 3 でのバックグラウンドスレッド化**: モデルロード（数秒〜数十秒）が追加されると
> メインスレッドブロックが問題になる。Swift 側の呼び出しコードを Phase 2 から
> `DispatchQueue.global().async` にしておくことで、Phase 3 での変更が Swift 側不要になる。

### `karukan_session_free`

```rust
/// セッションを解放する（学習キャッシュを保存してから drop）
#[unsafe(no_mangle)]
pub extern "C" fn karukan_session_free(session: *mut KarukanSession) {
    std::panic::catch_unwind(|| {
        if session.is_null() { return; }
        let mut s = unsafe { Box::from_raw(session) };
        s.save_learning();  // 学習キャッシュを保存
        // Box::drop で KarukanSession が解放される
    })
    .ok();  // panic でも leak しない（Box::from_raw は already-owned）
}
```

---

## `ffi/input.rs` — 文字・キー入力

### `KarukanKey` enum

```c
/* karukan_macos.h */
typedef enum {
    KARUKAN_KEY_RETURN    = 1,
    KARUKAN_KEY_BACKSPACE = 2,
    KARUKAN_KEY_ESCAPE    = 3,
    KARUKAN_KEY_SPACE     = 4,
    KARUKAN_KEY_LEFT      = 5,
    KARUKAN_KEY_RIGHT     = 6,
    KARUKAN_KEY_UP        = 7,
    KARUKAN_KEY_DOWN      = 8,
    KARUKAN_KEY_TAB       = 9,
} KarukanKey;
```

```rust
// session.rs に定義
#[repr(u32)]
pub enum KarukanKey {
    Return    = 1,
    Backspace = 2,
    Escape    = 3,
    Space     = 4,
    Left      = 5,
    Right     = 6,
    Up        = 7,
    Down      = 8,
    Tab       = 9,
}

impl KarukanKey {
    fn from_u32(v: u32) -> Option<Self> {
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
```

### `karukan_push_char`

```rust
/// 印字可能文字を push する（ローマ字1文字）
///
/// c: UTF-8 の null 終端文字列。1 Unicodeコードポイントのみ受け付ける
/// 戻り値: 1=IMEが消費, 0=スルー（IMEに入力がない状態で特殊キーが来た場合等）
#[unsafe(no_mangle)]
pub extern "C" fn karukan_push_char(
    session: *mut KarukanSession,
    c: *const c_char,
) -> c_int {
    std::panic::catch_unwind(|| {
        let session = ffi_mut!(session, 0);
        if c.is_null() { return 0; }

        let s = unsafe {
            match std::ffi::CStr::from_ptr(c).to_str() {
                Ok(s) => s,
                Err(_) => return 0,
            }
        };

        // 最初の1コードポイントだけ取る
        let ch = match s.chars().next() {
            Some(ch) if !ch.is_control() => ch,
            _ => return 0,
        };

        if session.push_char(ch) { 1 } else { 0 }
    })
    .unwrap_or(0)
}
```

### `karukan_push_key`

```rust
/// 特殊キーを push する
///
/// key: KarukanKey enum 値
/// 戻り値: 1=IMEが消費, 0=スルー
#[unsafe(no_mangle)]
pub extern "C" fn karukan_push_key(
    session: *mut KarukanSession,
    key: u32,
) -> c_int {
    std::panic::catch_unwind(|| {
        let session = ffi_mut!(session, 0);
        let key = match KarukanSession::key_from_u32(key) {
            Some(k) => k,
            None => return 0,
        };
        if session.push_key(key) { 1 } else { 0 }
    })
    .unwrap_or(0)
}
```

---

## `ffi/query.rs` — 状態取得

```rust
/// preedit テキストへのポインタを返す
/// ポインタは次の push_* 呼び出しまで有効
/// Swift 側は String(cString:) で即コピーすること
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit(session: *const KarukanSession) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.preedit.text.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit_len(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, 0);
        s.preedit.text.as_bytes().len() as u32
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_preedit_caret(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| {
        ffi_ref!(session, 0).preedit.caret_bytes
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_has_commit(session: *const KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        if ffi_ref!(session, 0).commit.dirty { 1 } else { 0 }
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_commit(session: *const KarukanSession) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.commit.text.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_is_empty(session: *const KarukanSession) -> c_int {
    std::panic::catch_unwind(|| {
        if ffi_ref!(session, 1).is_empty() { 1 } else { 0 }
    })
    .unwrap_or(1)
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_save_learning(session: *mut KarukanSession) {
    std::panic::catch_unwind(|| {
        ffi_mut!(session).save_learning();
    })
    .ok();
}
```

### `KarukanSession::save_learning()` の実装

```rust
impl KarukanSession {
    pub fn save_learning(&mut self) {
        let Some(cache) = &mut self.learning else { return };
        if !cache.is_dirty() { return; }

        let path = crate::platform::paths::learning_cache_path();
        // 親ディレクトリを作成
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = cache.save(&path) {
            tracing::warn!("Failed to save learning cache: {}", e);
        } else {
            tracing::debug!("Learning cache saved to {:?}", path);
        }
    }
}
```

---

## Cヘッダーファイル (`karukan-macos/include/karukan_macos.h`)

Swift との FFI ブリッジに使用する。

```c
#ifndef KARUKAN_MACOS_H
#define KARUKAN_MACOS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct KarukanSession KarukanSession;

typedef enum {
    KARUKAN_KEY_RETURN    = 1,
    KARUKAN_KEY_BACKSPACE = 2,
    KARUKAN_KEY_ESCAPE    = 3,
    KARUKAN_KEY_SPACE     = 4,
    KARUKAN_KEY_LEFT      = 5,
    KARUKAN_KEY_RIGHT     = 6,
    KARUKAN_KEY_UP        = 7,
    KARUKAN_KEY_DOWN      = 8,
    KARUKAN_KEY_TAB       = 9,
} KarukanKey;

/* --- ライフサイクル --- */
KarukanSession* karukan_session_new(void);
int             karukan_session_init(KarukanSession* session);
void            karukan_session_free(KarukanSession* session);

/* --- 入力 --- */
/* 戻り値: 1=消費, 0=スルー */
int karukan_push_char(KarukanSession* session, const char* c);
int karukan_push_key(KarukanSession* session, uint32_t key);

/* --- 状態取得（ポインタは次の push_* まで有効） --- */
const char* karukan_get_preedit(const KarukanSession* session);
uint32_t    karukan_get_preedit_len(const KarukanSession* session);
uint32_t    karukan_get_preedit_caret(const KarukanSession* session);

int         karukan_has_commit(const KarukanSession* session);
const char* karukan_get_commit(const KarukanSession* session);

/* Phase 3 で追加予定（ヘッダーには先行宣言しない） */
/* uint32_t karukan_get_candidate_count(...); */

int  karukan_is_empty(const KarukanSession* session);
void karukan_save_learning(KarukanSession* session);

#ifdef __cplusplus
}
#endif

#endif /* KARUKAN_MACOS_H */
```

---

## `ffi/tests.rs` — FFI 境界テスト

テストは `cargo test -p karukan-macos` で実行する。`KARUKAN_DATA_DIR` を tempdir に設定してファイルシステムを汚染しない。

### テストケース一覧

| テスト名 | 検証内容 |
|---|---|
| `test_session_lifecycle` | `new` → `init` → `free` がクラッシュしない |
| `test_null_safety` | 全関数に null を渡してもクラッシュしない |
| `test_basic_romaji_a` | `push_char("a")` → preedit == "あ" |
| `test_romaji_ka` | `push_char("k")` → preedit == "k", `push_char("a")` → preedit == "か" |
| `test_commit_on_return` | "a" + RETURN → `has_commit` == 1, commit == "あ" |
| `test_escape_cancel` | "ai" + ESCAPE → preedit_len == 0, `has_commit` == 0 |
| `test_backspace_romaji_buf` | "k" + BACKSPACE → preedit_len == 0 |
| `test_backspace_hiragana` | "ka" + BACKSPACE → preedit == "k" ... → preedit_len == 0 |
| `test_is_empty_after_cancel` | ESCAPE後に `is_empty` == 1 |
| `test_preedit_valid_utf8` | preedit ポインタが valid null-terminated UTF-8 |
| `test_preedit_pointer_stability` | 次の push_* 前はポインタが変化しない |
| `test_panic_safety` | 意図的 panic が FFI 境界を越えないこと（catch_unwind で処理） |
| `test_save_learning_no_crash` | `save_learning` が tempdir に保存してクラッシュしない |

### テストコードの骨格

```rust
// ffi/tests.rs
use super::{lifecycle::*, input::*, query::*};
use std::ffi::{CStr, CString};

struct TestSession(*mut KarukanSession);

impl TestSession {
    fn new() -> Self {
        std::env::set_var("KARUKAN_DATA_DIR", tempfile::tempdir().unwrap().path());
        let p = karukan_session_new();
        assert!(!p.is_null());
        // init は Phase 1 では軽量なので同期で呼んでよい
        assert_eq!(karukan_session_init(p), 0);
        Self(p)
    }

    fn push_char(&self, c: &str) -> bool {
        let cs = CString::new(c).unwrap();
        unsafe { karukan_push_char(self.0, cs.as_ptr()) == 1 }
    }

    fn push_key(&self, key: u32) -> bool {
        unsafe { karukan_push_key(self.0, key) == 1 }
    }

    fn preedit(&self) -> &str {
        let ptr = unsafe { karukan_get_preedit(self.0) };
        if ptr.is_null() { return ""; }
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }

    fn commit_text(&self) -> &str {
        let ptr = unsafe { karukan_get_commit(self.0) };
        if ptr.is_null() { return ""; }
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }
}

impl Drop for TestSession {
    fn drop(&mut self) { unsafe { karukan_session_free(self.0); } }
}

#[test]
fn test_basic_romaji_a() {
    let s = TestSession::new();
    s.push_char("a");
    assert_eq!(s.preedit(), "あ");
}

#[test]
fn test_null_safety() {
    use std::ptr;
    // null に渡しても panic しない
    unsafe {
        assert_eq!(karukan_push_char(ptr::null_mut(), ptr::null()), 0);
        assert_eq!(karukan_push_key(ptr::null_mut(), 1), 0);
        assert!(karukan_get_preedit(ptr::null()).is_null());
        assert_eq!(karukan_get_preedit_len(ptr::null()), 0);
        assert_eq!(karukan_has_commit(ptr::null()), 0);
        assert_eq!(karukan_is_empty(ptr::null()), 1);
        karukan_session_free(ptr::null_mut());  // no crash
        karukan_save_learning(ptr::null_mut()); // no crash
    }
}

// ... 他のテストケースは実装者が追加
```

---

## ワークスペースへの追加

```toml
# ルート Cargo.toml
[workspace]
members = [
    "karukan-engine",
    "karukan-cli",
    "karukan-im",
    "karukan-macos",   # ← 追加
]
```

---

## 実装上の注意事項

### `RomajiConverter` の公開 API（確認済み）

`karukan-engine/src/romaji/converter.rs` を直接確認した結果を以下に記す。
実装時はこの情報に従うこと（`cargo doc` での再確認は不要）。

#### メソッド一覧

| メソッド | シグネチャ | 説明 |
|---|---|---|
| `push` | `fn push(&mut self, ch: char) -> ConversionEvent` | 文字を追加して変換試行。**内部で `to_ascii_lowercase()` するため大文字も受け付ける** |
| `backspace` | `fn backspace(&mut self) -> BackspaceResult` | 1文字削除。下記 variant 参照 |
| `flush` | `fn flush(&mut self) -> String` | バッファ残留分を強制変換して返す。同時に `output` にも追記される |
| `buffer` | `fn buffer(&self) -> &str` | 未確定ローマ字バッファ（例: "k", "sh"） |
| `output` | `fn output(&self) -> &str` | **累積値**。`reset()` するまで追記され続ける |
| `reset` | `fn reset(&mut self)` | `buffer` と `output` を両方クリア |
| `full_text` | `fn full_text(&self) -> String` | `output + buffer` を結合した文字列 |

#### `ConversionEvent` variants

```rust
pub enum ConversionEvent {
    Converted(String),   // ひらがなに変換された（例: "ka" → "か"）
    Buffered,            // バッファに追加されて待機中（例: "k"）
    PassThrough(char),   // 変換ルールなし（例: "?" → "？" 等は実はルールあり）
}
```

#### `BackspaceResult` variants（計画書初版の `Consumed`/`NotConsumed` は誤り）

```rust
pub enum BackspaceResult {
    RemovedBuffer(char),  // 未確定ローマ字バッファから削除した
    RemovedOutput(char),  // 確定済み output から削除した（= input_buf からも削除が必要）
    Empty,                // バッファも output も空だった
}
```

> **`RemovedOutput` 時の注意**: `romaji.output` から文字が消えた場合、
> その文字は `push_char()` 内の delta 追跡で `input_buf` に既に `insert()` 済み。
> `do_backspace()` では `RemovedOutput` を受け取ったら `input_buf.delete_before_cursor()` も呼ぶ必要がある。
> （本計画書の `do_backspace()` 実装指針はこの仕様を反映済み）

#### `output()` の累積仕様と delta 追跡

`output()` は毎 push で追記される累積値のため、`push_char()` では以下の delta 追跡が必要：

```rust
let prev_output_len = self.romaji.output().chars().count();
self.romaji.push(ch);
let new_hiragana: String = self.romaji.output().chars().skip(prev_output_len).collect();
// new_hiragana だけを input_buf に insert する
```

### `catch_unwind` の制限

`catch_unwind` は `UnwindSafe` を満たす型にのみ使える。
クロージャ内で `&mut KarukanSession` を使う場合、`AssertUnwindSafe` でラップが必要になることがある：

```rust
std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let session = ffi_mut!(session, 0);
    // ...
}))
```

### `cdylib` + `lib` の同時指定

`crate-type = ["cdylib", "lib"]` にすると：

- `cargo build` → `libkarukan_macos.dylib` （Xcodeに埋め込む）
- `cargo test` → rlib ベースのテストバイナリ（`#[test]` が動く）

`cdylib` のみだと `cargo test` が動かないため必ず両方指定する。

---

## 完了条件（acceptance criteria）

### Rust ビルド・テスト

- [ ] `cargo build -p karukan-macos --release` が成功する
- [ ] `cargo test -p karukan-macos` が全パスする
- [ ] `cargo clippy -p karukan-macos` が警告なし
- [ ] `cargo fmt -p karukan-macos -- --check` が通る
- [ ] `include/karukan_macos.h` が存在し、全公開関数の宣言が含まれる
- [ ] null ポインタテストが全パスする（クラッシュゼロ）

### dylib 検査（Phase 2 の Xcode 統合前に問題を潰す）

Xcode への埋め込み可否は Phase 2 まで確認できないが、以下を Phase 1 で検証して
「埋め込み可能な状態」を保証する。

#### シンボルエクスポート確認

```bash
nm -D target/release/libkarukan_macos.dylib | grep karukan_
```

期待出力（全公開関数が `T` セクションに現れること）:

```text
T _karukan_get_commit
T _karukan_get_preedit
T _karukan_get_preedit_caret
T _karukan_get_preedit_len
T _karukan_has_commit
T _karukan_is_empty
T _karukan_push_char
T _karukan_push_key
T _karukan_save_learning
T _karukan_session_free
T _karukan_session_init
T _karukan_session_new
```

#### dylib ID と依存関係確認

```bash
# dylib 自身の ID を確認（@rpath に設定されていること）
otool -D target/release/libkarukan_macos.dylib

# 依存ライブラリを確認（Linux 固有のものが含まれていないこと）
otool -L target/release/libkarukan_macos.dylib
```

期待: `otool -D` の出力が `libkarukan_macos.dylib`（絶対パスなし）であること。
Xcode の Build Phase で `install_name_tool -id "@rpath/libkarukan_macos.dylib"` を適用する前提のため、
Phase 1 時点でのデフォルト ID は絶対パスでも問題ないが、Linux 固有の依存（`libpthread.so` 等）が
混入していないことを確認すること。

**`dlopen` スモークテスト（最小 C プログラム）**

```c
// smoke_test.c
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
#include "include/karukan_macos.h"

int main(void) {
    void *lib = dlopen("target/release/libkarukan_macos.dylib", RTLD_NOW);
    if (!lib) { fprintf(stderr, "dlopen failed: %s\n", dlerror()); return 1; }

    KarukanSession* (*new_fn)(void) = dlsym(lib, "karukan_session_new");
    int (*push_fn)(KarukanSession*, const char*) = dlsym(lib, "karukan_push_char");
    const char* (*preedit_fn)(const KarukanSession*) = dlsym(lib, "karukan_get_preedit");
    void (*free_fn)(KarukanSession*) = dlsym(lib, "karukan_session_free");

    KarukanSession *s = new_fn();
    push_fn(s, "a");
    const char *p = preedit_fn(s);
    printf("preedit: %s\n", p);
    int ok = strcmp(p, "あ") == 0;
    free_fn(s);
    dlclose(lib);
    return ok ? 0 : 1;
}
```

```bash
clang smoke_test.c -o smoke_test
./smoke_test   # 終了コード 0 かつ "preedit: あ" が出力されること
```

- [ ] `nm -D` で全公開シンボルが確認できる
- [ ] `otool -L` に Linux 固有の依存がない
- [ ] `dlopen` スモークテストが成功する（`push_char("a")` → preedit `"あ"`）

> **Xcode 統合での残存リスク**: `.appex` バンドルへの埋め込み後の `@rpath` 解決、
> および `install_name_tool` 適用後の動作確認は Phase 2 で行う。

---

## Phase 2 への引き継ぎ事項

Phase 1 完了時に以下の状態であること：

1. `libkarukan_macos.dylib` が `target/release/` に生成されている
2. `include/karukan_macos.h` が存在する
3. `karukan_push_key(KARUKAN_KEY_SPACE)` は現在「全角スペースとして確定」する（Phase 3 で変換トリガーに変更）
4. `karukan_session_init` は辞書・学習キャッシュのロードのみ（モデルロードは Phase 3 で追加）
5. Swift 側は Phase 2 から `DispatchQueue.global().async { karukan_session_init(session) }` で呼ぶように実装する（Phase 3 のモデルロード追加に対応するため）
