# karukan macOS 対応 開発計画書

> 作成日: 2026-02-24
> 対象: macOS 15 (Sequoia) 以降
> ツール: Xcode 26.3 RC2、Rust 1.92+

---

## 目的

`karukan-engine` を変更せずに、macOS InputMethodKit 上で動作する日本語IMEを実装する。

---

## 絶対制約

| # | 制約 |
|---|---|
| 1 | `karukan-engine` は変更禁止 |
| 2 | Rust FFI クレートは `cdylib` |
| 3 | C ABI (`extern "C"`) |
| 4 | panic を FFI 境界で跨がせない（`catch_unwind` 必須） |
| 5 | 文字列は UTF-8 C文字列（null終端） |
| 6 | 所有権は Rust 側管理（Swift 側は `copy_string` して使う） |
| 7 | Swift にロジックを置かない |
| 8 | モデル初期化はバックグラウンドスレッドで行う（メインスレッドブロック禁止） |
| 9 | macOS 15+ 対応 |
| 10 | Xcode 26.3 RC2 を利用する |

> **制約8の補足**: 旧計画の「同期モデルのみ（async禁止）」は、IMKがメインスレッドで動作するため、
> モデルロードでOS全体がフリーズする。`karukan_session_new` と `karukan_session_init` を分離し、
> init はバックグラウンドスレッドで呼ぶ設計とする。Rust側APIは同期だが、呼び出し側（Swift）が
> スレッドを管理する。

---

## リポジトリ構造

既存 `karukan-im`（Linux/fcitx5専用）はそのまま維持する。macOS向けに新クレートを並列追加する。

```text
karukan/
├── karukan-engine/          # 変更禁止
├── karukan-cli/             # 変更禁止
├── karukan-im/              # 変更禁止（Linux/fcitx5専用）
├── karukan-macos/           # 新規 cdylib クレート ← Phase 1
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── session.rs       # KarukanSession opaque 型
│       ├── ffi/
│       │   ├── mod.rs
│       │   ├── lifecycle.rs # session_new / session_free / session_init
│       │   ├── input.rs     # push_char / push_key
│       │   └── query.rs     # get_preedit / get_candidates 等
│       └── platform/
│           └── paths.rs     # macOS データディレクトリ解決
└── macos/                   # Xcode プロジェクト ← Phase 2〜4
    ├── KarukanIM.xcodeproj
    ├── KarukanIM/           # ホストアプリ（配布に必要）
    │   └── ...
    ├── KarukanIMExtension/  # Input Method App Extension
    │   ├── KarukanInputController.swift
    │   ├── Info.plist
    │   └── KarukanIM.entitlements
    └── docs/
        └── develop-plan.md  # 本ファイル
```

### なぜ `karukan-macos` を新規作成するか

`karukan-im/src/ffi/` に既存FFIが存在するが、以下の理由でmacOSには直接使えない：

1. **Keycodeの非互換**: 既存FFIはXKB/X11 keysym（`0xff08` 等）を使用。macOS IMKは Carbon key codes を渡す。完全に異なる体系。
2. **データパス非互換**: `~/.local/share/karukan-im/` 等のXDGパスはmacOSに存在しない。
3. **`catch_unwind` 未実装**: 既存FFIにはpanic保護がない。新クレートで全関数に実装する。
4. **Linux依存の排除**: `karukan-im` はfcitx5向けの設定・初期化ロジックを含む。macOS向けに不要な依存を持ち込まない。

---

## フェーズ概要

| Phase | 内容 | 状態 |
|---|---|---|
| 1 | Rust FFI クレート (`karukan-macos`) | ✅ 完了 |
| 2 | InputMethodKit 最小実装（ローマ字→ひらがな） | ✅ 完了 |
| 3 | 漢字変換 + 候補UI統合 | ✅ 完了 |
| 4 | ライブ変換の洗練 | ✅ 完了 |
| 5 | キーボードショートカット・インジケーターアイコン・設定基盤 | ✅ 完了（`develop-phase5.md`） |
| 6 | 設定アプリ（SwiftUI 独立アプリ） | 📋 計画済み（`develop-phase6.md`） |
| 7 | Universal Binary・公証・配布 | 未着手 |

各フェーズ完了後に `epic/macos` へマージ。次フェーズへの go/nogo は開発者が判断する。

---

## Phase 1: Rust FFI クレート (`karukan-macos`)

### ゴール

`karukan-engine` を macOS 向けにラップした `cdylib` を作成する。

### ディレクトリ・ファイル

```text
karukan-macos/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── session.rs
    ├── platform/
    │   └── paths.rs
    └── ffi/
        ├── mod.rs       # ffi_ref!/ffi_mut! マクロ、共通ユーティリティ
        ├── lifecycle.rs # new / init / free
        ├── input.rs     # push_char / push_key
        └── query.rs     # preedit / candidates / commit 取得
```

### API 設計

#### セッション管理

```c
// セッション生成（軽量・メインスレッドOK）
KarukanSession* karukan_session_new(void);

// モデルロード（重い・バックグラウンドスレッドで呼ぶ）
// 戻り値: 0=成功, -1=失敗
int karukan_session_init(KarukanSession* session);

// 解放（学習キャッシュを保存してdrop）
void karukan_session_free(KarukanSession* session);
```

#### 文字入力

```c
// 印字可能文字をpush（ローマ字1文字ずつ）
// c: UTF-8 1コードポイント (null終端)
// 戻り値: 1=IMEが消費, 0=スルー
int karukan_push_char(KarukanSession* session, const char* c);

// 特殊キー入力
// key: KarukanKey enum 値（下記）
// 戻り値: 1=IMEが消費, 0=スルー
int karukan_push_key(KarukanSession* session, uint32_t key);
```

```c
// macOSネイティブのキーコード体系（X11 keysym ではない）
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

> **設計注記**: 既存 `karukan-im` の `karukan_engine_process_key(keysym, state, is_release)` はX11keysymベース。
> macOSでは `karukan_push_char` + `karukan_push_key` の2関数に分離し、macOS側でのキーマッピングを不要にする。

#### 状態取得（ポインタは次の push_* 呼び出しまで有効）

```c
// preedit テキスト（null終端 UTF-8）
const char* karukan_get_preedit(const KarukanSession* session);
uint32_t    karukan_get_preedit_len(const KarukanSession* session);
uint32_t    karukan_get_preedit_caret(const KarukanSession* session);  // バイトオフセット

// コミットテキスト
int         karukan_has_commit(const KarukanSession* session);
const char* karukan_get_commit(const KarukanSession* session);

// 候補
uint32_t    karukan_get_candidate_count(const KarukanSession* session);
const char* karukan_get_candidate(const KarukanSession* session, uint32_t index);
const char* karukan_get_candidate_annotation(const KarukanSession* session, uint32_t index);
uint32_t    karukan_get_candidate_cursor(const KarukanSession* session);

// 空状態確認
int         karukan_is_empty(const KarukanSession* session);

// 学習キャッシュ保存（フォーカス喪失時に呼ぶ）
void        karukan_save_learning(KarukanSession* session);
```

> **所有権ポリシー**: 返却ポインタはRust側が所有・管理する。
> Swift側は取得後すぐに `String(cString:)` でコピーすること。
> `karukan_string_free` は提供しない（Rust側キャッシュの寿命で自動管理）。

### macOS データパス

**XDG ではなく macOS 標準パス（`~/Library/Application Support/`）を採用する。**

`karukan-im`（Linux）は XDG パス（`~/.local/share/karukan-im/`）を使用しているが、
macOS の Input Method Extension には以下の理由から XDG は適用できない。

| 観点 | XDG | macOS 標準 |
|---|---|---|
| 環境変数の継承 | シェル起動 → ユーザー環境から継承 | **macOS がシステムとして起動 → 環境変数なし** |
| `$XDG_CONFIG_HOME` | シェルで設定可 | IME プロセスには未設定・到達しない |
| サンドボックス対応 | `~/.config/` はコンテナ外 → **アクセス不可** | `~/Library/Application Support/` は自動リダイレクト ✓ |
| Finder / Time Machine | 不可視・管理しにくい | 可視・除外設定が容易 |

> **サンドボックスとパスの関係**: 将来 App Sandbox を有効化した場合、
> `~/Library/Application Support/Karukan/` は自動的に
> `~/Library/Containers/<bundle-id>/Data/Library/Application Support/Karukan/` に
> リダイレクトされ、コードの変更なく動作する。
> `~/.config/` や `~/.local/share/` はコンテナ外のため即座に Permission Denied になる。

テストおよび CI では `KARUKAN_DATA_DIR` 環境変数でパスをオーバーライドできるようにし、
XDG 的な使い方も開発時に限り可能とする（本番 IME プロセスでは無視される）。

```rust
// platform/paths.rs
pub fn app_support_dir() -> PathBuf {
    // テスト・CI 用オーバーライド
    if let Ok(p) = std::env::var("KARUKAN_DATA_DIR") {
        return PathBuf::from(p);
    }
    // macOS 標準パス（本番）
    // ~/Library/Application Support/Karukan/
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("Karukan")
}

// ~/Library/Application Support/Karukan/learning.tsv
pub fn learning_cache_path() -> PathBuf { ... }

// ~/Library/Application Support/Karukan/models/
pub fn models_dir() -> PathBuf { ... }

// ~/Library/Application Support/Karukan/user_dict.txt
pub fn user_dict_path() -> PathBuf { ... }
```

### Metal バックエンド

macOSのGPUアクセラレーションにはllama-cppのMetalバックエンドが必要。

```toml
# karukan-macos/Cargo.toml
[dependencies]
karukan-engine.workspace = true

[target.'cfg(target_os = "macos")'.dependencies]
llama-cpp-2 = { version = "0.1", features = ["metal"] }
```

### panic 保護

全 `extern "C"` 関数に `catch_unwind` を適用する（既存`karukan-im` FFIには存在しないため、本クレートで新規実装）。

```rust
// パターン例
#[unsafe(no_mangle)]
pub extern "C" fn karukan_push_char(session: *mut KarukanSession, c: *const c_char) -> c_int {
    std::panic::catch_unwind(|| {
        let session = ffi_mut!(session, 0);
        // ... 実装
    })
    .unwrap_or(0)  // panic時は安全なデフォルト値を返す
}
```

### テスト要件

- `karukan_session_new` → `karukan_session_init` → `karukan_push_char` → `karukan_get_preedit` の一連フロー
- ローマ字→ひらがな変換: `k` → preedit="k"、`a` → preedit="か"
- Backspace動作
- panic が FFI 境界を越えないことの確認

### Cargo.toml ワークスペース追加

```toml
# ルートの Cargo.toml
[workspace]
members = ["karukan-engine", "karukan-cli", "karukan-im", "karukan-macos"]
```

---

## Phase 2: macOS InputMethodKit 最小実装

### ゴール

ローマ字→ひらがな入力を macOS アプリケーション上で動作させる。

### App Bundle 構造

macOS の Input Method は、**含有アプリ（Hosting App）の PlugIns/ 内に配置する App Extension**として動作する。`.appex` 単体では配布・インストールできない。

```text
KarukanIM.app/
└── Contents/
    ├── MacOS/
    │   └── KarukanIM          # ホストアプリ本体（UI無し可）
    ├── Info.plist
    └── PlugIns/
        └── KarukanIMExtension.appex/
            └── Contents/
                ├── MacOS/
                │   └── KarukanIMExtension
                ├── Frameworks/
                │   └── libkarukan_macos.dylib   # Rust cdylib
                └── Info.plist
```

### Xcode プロジェクト設定

- **Target 1**: `KarukanIM` (macOS App, ホストアプリ)
  - LSUIElement = YES（Dockに表示しない）
  - 最小限のSwiftUI画面 or 空のNSApplicationDelegate
- **Target 2**: `KarukanIMExtension` (Input Method Extension)
  - Extension Type: `com.apple.inputmethodkit`
  - Principal Class: `KarukanInputController`
  - Embed `libkarukan_macos.dylib` in Frameworks

### `Info.plist` キー（Extension）

```xml
<key>InputMethodConnectionName</key>
<string>KarukanIM</string>
<key>InputMethodServerControllerClass</key>
<string>KarukanInputController</string>
<key>NSPrincipalClass</key>
<string>KarukanInputController</string>
<key>ComponentInputModeDict</key>
<dict>
    <key>tsInputModeListKey</key>
    <dict>
        <key>com.example.inputmethod.karukan.hiragana</key>
        <dict>
            <key>TISInputSourceIsASCIICapable</key>
            <false/>
            <key>TISInputSourceType</key>
            <string>com.apple.input-method.Roman</string>
        </dict>
    </dict>
</dict>
```

### `KarukanInputController.swift` 設計方針

```text
IMKInputController
├── init(server:delegate:client:)
│   └── karukan_session_new()
│       └── DispatchQueue.global().async { karukan_session_init() }
│           └── 初期化完了後フラグ更新
│
├── inputText(_:key:modifiers:client:) → Bool
│   ├── 特殊キー → karukan_push_key()
│   ├── 印字可能文字 → karukan_push_char()
│   ├── has_commit → client.insertText()
│   └── preedit更新 → client.setMarkedText()
│
└── deactivateServer(_:)
    └── karukan_save_learning()
```

**Swift側のロジック禁止**: IMKのコールバックは引数の振り分けのみ行い、変換ロジックはすべてRust側に委譲する。

### dylib 組み込み

`karukan-macos` はワークスペースのビルド成果物 `libkarukan_macos.dylib` を Xcode の Build Phase でコピーする。
Xcode Build Phase スクリプト例:

```bash
# Xcode の "Run Script" Build Phase
RUST_TARGET_DIR="${SRCROOT}/../target/release"
cp "${RUST_TARGET_DIR}/libkarukan_macos.dylib" \
   "${BUILT_PRODUCTS_DIR}/${FRAMEWORKS_FOLDER_PATH}/libkarukan_macos.dylib"
install_name_tool -id "@rpath/libkarukan_macos.dylib" \
   "${BUILT_PRODUCTS_DIR}/${FRAMEWORKS_FOLDER_PATH}/libkarukan_macos.dylib"
```

### テスト要件

- IME登録（システム設定 > キーボード > 入力ソース）後に有効化できること
- ローマ字入力でpreedit（下線付きひらがな）が表示されること
- Enter でひらがながコミットされること

---

## Phase 3: 漢字変換 + 候補UI統合

### ゴール

スペースキーで変換候補を表示し、選択してコミットできるようにする。

### Rust 側

Phase 1 で既に `karukan_push_key(KARUKAN_KEY_SPACE)` が変換トリガーとなるよう実装する。
`karukan_engine` の `InputMethodEngine` はスペースキーで変換フローに入り、
`ShowCandidates` アクションを生成する設計（`karukan-im` の既存実装を踏襲）。

`karukan_get_candidate_count` / `karukan_get_candidate` でSwift側から候補を取得する。

### Swift 側（`IMKCandidates` 使用）

```text
Space キー押下
    ↓
karukan_push_key(KARUKAN_KEY_SPACE)
    ↓
karukan_get_candidate_count() > 0
    ↓
IMKCandidates.update(candidates: [String])
    ↓
IMKCandidates.show(.horizontalCandidates)
    ↓
候補選択
    ↓
IMKInputController.candidateSelected(_:)
    ↓
karukan_push_key(KARUKAN_KEY_RETURN)  ← 選択番号指定版を追加検討
    ↓
karukan_has_commit() → client.insertText()
    ↓
バッファクリア（Rust側が管理）
```

### セッション単位の状態独立

`KarukanSession` は `InputMethodEngine` を1:1で保持。アプリ切り替え時は新セッションを生成することで状態が独立する。

### キャンセル動作

Escape キー → `karukan_push_key(KARUKAN_KEY_ESCAPE)` → バッファクリア → preedit消去。

### テスト要件

- スペースで候補ウィンドウが表示されること
- 候補選択で確定テキストがコミットされること
- Escape でキャンセルされpreeditが消えること
- commit後にバッファがクリアされること

---

## Phase 4: Universal Binary・Hardened Runtime・公証・配布

### ゴール

Gatekeeper を通過し、一般ユーザーが安全にインストールできる形式で配布する。

### 1. Universal Binary ビルド

```bash
# Rust cdylib のユニバーサルバイナリ作成
cargo build -p karukan-macos --release --target aarch64-apple-darwin
cargo build -p karukan-macos --release --target x86_64-apple-darwin

lipo -create \
  target/aarch64-apple-darwin/release/libkarukan_macos.dylib \
  target/x86_64-apple-darwin/release/libkarukan_macos.dylib \
  -output macos/universal/libkarukan_macos.dylib
```

Xcode 側は **Architectures: Standard Architectures (arm64, x86_64)** に設定。

### 2. Hardened Runtime + Entitlements

Hardened Runtime は公証の必須条件。Input Method Extension に必要な entitlements：

```xml
<!-- KarukanIM.entitlements -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" ...>
<plist version="1.0">
<dict>
    <!-- Hardened Runtime 基本設定 -->
    <key>com.apple.security.cs.allow-jit</key>
    <false/>

    <!-- llama.cpp / Metal GPU アクセス -->
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>

    <!-- モデルDL（HuggingFace） -->
    <key>com.apple.security.network.client</key>
    <true/>

    <!-- モデル・学習キャッシュ保存 -->
    <!-- ~/Library/Application Support/Karukan/ は App Container 内なので追加entitlements不要 -->
</dict>
</plist>
```

> **サンドボックス注記**: Input Method Extension はフルサンドボックスではないが、
> Hardened Runtime を有効化した状態でllama.cppがJITや実行可能メモリを使う場合は
> `allow-unsigned-executable-memory` が必要。要動作確認。

### 3. コード署名

```bash
# dylib 署名
codesign --deep --force --options runtime \
  --entitlements macos/KarukanIMExtension/KarukanIM.entitlements \
  --sign "Developer ID Application: YOUR_NAME (TEAM_ID)" \
  macos/universal/libkarukan_macos.dylib

# appex 署名
codesign --deep --force --options runtime \
  --entitlements macos/KarukanIMExtension/KarukanIM.entitlements \
  --sign "Developer ID Application: YOUR_NAME (TEAM_ID)" \
  "build/KarukanIM.app/Contents/PlugIns/KarukanIMExtension.appex"

# ホストアプリ署名
codesign --deep --force --options runtime \
  --sign "Developer ID Application: YOUR_NAME (TEAM_ID)" \
  "build/KarukanIM.app"
```

### 4. 公証 (Notarization)

```bash
# zip を作成して submit
ditto -c -k --keepParent "build/KarukanIM.app" "KarukanIM.zip"

xcrun notarytool submit KarukanIM.zip \
  --apple-id "YOUR_APPLE_ID" \
  --team-id "TEAM_ID" \
  --password "APP_SPECIFIC_PASSWORD" \
  --wait
```

### 5. Staple

```bash
xcrun stapler staple "build/KarukanIM.app"
xcrun stapler validate "build/KarukanIM.app"
```

### 6. 配布形式（pkg）

IMEはシステムディレクトリへのインストールが必要なため `pkg` 形式を使用。

```bash
# pkg 作成
pkgbuild --component "build/KarukanIM.app" \
  --install-location /Library/Input\ Methods \
  --sign "Developer ID Installer: YOUR_NAME (TEAM_ID)" \
  KarukanIM.pkg

# pkg 公証
xcrun notarytool submit KarukanIM.pkg \
  --apple-id "YOUR_APPLE_ID" \
  --team-id "TEAM_ID" \
  --password "APP_SPECIFIC_PASSWORD" \
  --wait

xcrun stapler staple KarukanIM.pkg
```

### テスト要件

- 公証後の pkg を未登録のMacにインストールしてGatekeeperが通過すること
- `spctl --assess --verbose /Library/Input\ Methods/KarukanIM.app` が `accepted` を返すこと
- arm64 / x86_64 双方で動作確認

---

## 既知リスク一覧

| # | リスク | 深刻度 | 対策 |
|---|---|---|---|
| R1 | llama.cppの `allow-unsigned-executable-memory` 要否不明 | 高 | Phase 1 完了後にmacOS上で動作確認。不要ならentitlementsから除外 |
| R2 | モデルDL（HuggingFace）がApp Extensionのネットワーク制限で失敗する可能性 | 高 | 初回DLをホストアプリ側で実施する設計に変更することを検討 |
| R3 | Metal featureがllama-cpp-2クレートで正常動作しない可能性 | 中 | `cargo test -p karukan-macos` でMetal推論の動作確認 |
| R4 | IMKCandidatesの挙動がmacOS バージョン間で異なる可能性 | 中 | macOS 15 + macOS 14 の両バージョンで確認 |
| R5 | `karukan_session_init` の完了前に入力が来た場合の処理 | 中 | 初期化完了フラグを設け、未完了時はキー入力をスルー |
| R6 | Xcode 26.3 RC2 の App Extension テンプレートの仕様変更 | 低 | 正式リリースで確認。RC2での差異は都度対応 |
| R7 | Universal Binary の lipo 後に dylib の rpath が壊れる | 低 | `install_name_tool` と `otool -L` で署名前に検証 |

---

## テスト戦略

| レベル | 内容 | フェーズ |
|---|---|---|
| Rust 単体テスト | `cargo test -p karukan-macos` | Phase 1 |
| FFI 結合テスト | C または Swift から FFI を呼び出す CLI ツールで検証 | Phase 1 |
| IME 動作確認 | macOS上でIME有効化 → ローマ字入力 → preedit表示 | Phase 2 |
| 変換・候補UI | スペースで候補表示 → 選択 → コミット | Phase 3 |
| Universal Binary | arm64 / x86_64 双方で変換動作確認 | Phase 4 |
| 公証通過確認 | pkg インストール後 Gatekeeper 通過 | Phase 4 |

---

## ビルドスクリプト概要

```bash
#!/usr/bin/env bash
# build.sh — Phase 4 向けフルビルドスクリプト（概要）

set -euo pipefail

# 1. Rust ユニバーサルバイナリ
cargo build -p karukan-macos --release --target aarch64-apple-darwin
cargo build -p karukan-macos --release --target x86_64-apple-darwin
mkdir -p macos/universal
lipo -create \
  target/aarch64-apple-darwin/release/libkarukan_macos.dylib \
  target/x86_64-apple-darwin/release/libkarukan_macos.dylib \
  -output macos/universal/libkarukan_macos.dylib

# 2. Xcode ビルド
xcodebuild -project macos/KarukanIM.xcodeproj \
  -scheme KarukanIM \
  -configuration Release \
  -archivePath build/KarukanIM.xcarchive \
  archive

xcodebuild -exportArchive \
  -archivePath build/KarukanIM.xcarchive \
  -exportOptionsPlist macos/ExportOptions.plist \
  -exportPath build/

# 3. 公証 → staple → pkg 作成
# (詳細は Phase 4 実装計画書を参照)
```

---

## 実装順序サマリー

```text
Phase 1: Rust FFI (karukan-macos)                    ✅
  └── Phase 2: IMKit 最小実装 (ローマ字→ひらがな)     ✅
        └── Phase 3: 漢字変換 + 候補UI                ✅
              └── Phase 4: ライブ変換の洗練            ✅
                    └── Phase 5: ショートカット・アイコン・設定基盤  ✅
                          └── Phase 6: 設定アプリ (SwiftUI)  📋 ← 現在
                                └── Phase 7: 公証・pkg 配布
                                      └── epic/macos → main マージ
```
