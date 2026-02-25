# Phase 2 実装計画書: InputMethodKit 最小実装

> ブランチ: `feature/macos-phase2-imk`
> 前提: `feature/macos-phase1-ffi` 完了・マージ済み
> 完了条件: macOS 上でローマ字→ひらがな入力が動作し、Enter でコミットされること

---

## 目標

Phase 1 で作成した `libkarukan_macos.dylib` を Xcode プロジェクトに組み込み、
macOS InputMethodKit (IMK) 上で日本語ローマ字入力を動作させる。

---

## スコープ

### Phase 2 でやること

- Xcode プロジェクト作成（ホストアプリ + App Extension）
- `libkarukan_macos.dylib` の `.appex` バンドルへの組み込みと `@rpath` 設定
- `KarukanInputController.swift` — IMK コールバックから Rust FFI への橋渡し
- ローマ字入力 → preedit（下線付き）表示
- Enter キー → ひらがなコミット
- Escape キー → 入力キャンセル
- Backspace キー → 1文字削除
- `karukan_session_init` をバックグラウンドスレッドで呼ぶ（Phase 3 対応）
- `deactivateServer` で `karukan_save_learning` 呼び出し
- IME 登録・有効化の動作確認

### Phase 2 でやらないこと（Phase 3 以降）

- 漢字変換（スペースキーは全角スペース確定のまま）
- 候補ウィンドウ（`IMKCandidates`）
- モデルのロード
- App Sandbox 有効化
- コード署名・公証

---

## 前提確認

Phase 2 開始前に以下を確認すること。

```bash
# Phase 1 の成果物が存在すること
ls -la target/release/libkarukan_macos.dylib
ls karukan-macos/include/karukan_macos.h

# 全シンボルがエクスポートされていること
nm -D target/release/libkarukan_macos.dylib | grep -E "^[0-9a-f]+ T _karukan_"
# 期待: 12 シンボルが T セクションに存在

# macOS 固有の依存のみであること
otool -L target/release/libkarukan_macos.dylib
# 期待: /usr/lib/libSystem.B.dylib、/System/Library/Frameworks/Metal.framework 等のみ
```

---

## 最終的なディレクトリ構造

```text
karukan/
├── karukan-macos/                 # Phase 1 成果物（変更なし）
│   ├── Cargo.toml
│   ├── include/
│   │   └── karukan_macos.h        # C ヘッダー（Swift からインクルード）
│   └── src/
│       └── ...
└── macos/                         # Phase 2 で新規作成
    ├── KarukanIM.xcodeproj
    ├── KarukanIM/                  # ホストアプリ（Target 1）
    │   ├── AppDelegate.swift
    │   ├── Assets.xcassets
    │   ├── Info.plist
    │   └── KarukanIM.entitlements
    ├── KarukanIMExtension/         # Input Method Extension（Target 2）
    │   ├── KarukanInputController.swift
    │   ├── KarukanBridge.h         # Objective-C Bridging Header
    │   ├── Info.plist
    │   └── KarukanIMExtension.entitlements
    └── docs/
        ├── develop-plan.md
        ├── develop-phase1.md
        └── develop-phase2.md       # 本ファイル
```

---

## 1. Xcode プロジェクト作成

### 1-1. プロジェクト作成

1. Xcode 26.3 RC2 を開く
2. **File > New > Project…**
3. **macOS > App** を選択
4. 以下を設定:
    - **Product Name**: `KarukanIM`
    - **Organization Identifier**: `com.example.karukan`（後で変更可）
    - **Bundle Identifier**: `com.example.karukan.KarukanIM`
    - **Language**: Swift
    - **User Interface**: SwiftUI（またはNone）
5. **Save location**: `karukan/macos/`（`karukan-macos/` と同階層）

> **注意**: Save 後、プロジェクトファイルが `macos/KarukanIM.xcodeproj` になることを確認する。

### 1-2. Extension ターゲット追加

1. **File > New > Target…**
2. **macOS > Input Method Extension** を選択
    - ない場合は **macOS > Generic Extension** を選択し、後述の Info.plist を手動設定
3. 以下を設定:
    - **Product Name**: `KarukanIMExtension`
    - **Bundle Identifier**: `com.example.karukan.KarukanIM.KarukanIMExtension`
4. **Activate** ダイアログが出たら **Cancel**（スキーム切り替えは手動で行う）

---

## 2. ターゲット設定

### 2-1. ホストアプリ（`KarukanIM`）

#### General

| 項目 | 値 |
|---|---|
| Deployment Target | macOS 15.0 |
| Bundle Identifier | `com.example.karukan.KarukanIM` |

#### Info.plist 追加キー

```xml
<!-- KarukanIM/Info.plist -->
<key>LSUIElement</key>
<true/>
<!-- Dock に表示しない。入力メソッドはバックグラウンドで動作する。 -->

<key>NSPrincipalClass</key>
<string>NSApplication</string>
```

#### AppDelegate.swift

```swift
// KarukanIM/AppDelegate.swift
import Cocoa

@main
class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // ホストアプリ本体はUI不要。
        // 将来 Phase 4 で設定ウィンドウを追加する。
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ app: NSApplication) -> Bool {
        return false
    }
}
```

#### Signing & Capabilities

- **Signing Certificate**: Sign to Run Locally（開発中）
- App Sandbox: **オフ**（Phase 4 で検討）

---

### 2-2. Extension ターゲット（`KarukanIMExtension`）

#### Build Settings

以下を `KarukanIMExtension` ターゲットの Build Settings に設定する。

| Build Setting | 値 | 備考 |
|---|---|---|
| `PRODUCT_NAME` | `KarukanIMExtension` | |
| `INFOPLIST_FILE` | `KarukanIMExtension/Info.plist` | |
| `SWIFT_OBJC_BRIDGING_HEADER` | `KarukanIMExtension/KarukanBridge.h` | C ヘッダーへのブリッジ |
| `LD_RUNPATH_SEARCH_PATHS` | `@loader_path/../Frameworks` | dylib の rpath 解決 |
| `MACOSX_DEPLOYMENT_TARGET` | `15.0` | |
| `SWIFT_VERSION` | `6.0` | |
| `OTHER_LDFLAGS` | （不要。スクリプトでコピー） | |

#### Signing & Capabilities

- **Signing Certificate**: Sign to Run Locally（開発中）
- App Sandbox: **オフ**

#### Info.plist（`KarukanIMExtension/Info.plist`）

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.example.karukan.KarukanIM.KarukanIMExtension</string>

    <key>CFBundleName</key>
    <string>Karukan</string>

    <key>CFBundleDisplayName</key>
    <string>Karukan</string>

    <key>NSPrincipalClass</key>
    <string>KarukanIMExtension.KarukanInputController</string>

    <!-- IMK 接続名: Info.plist の InputMethodConnectionName と一致させる -->
    <key>InputMethodConnectionName</key>
    <string>KarukanIM</string>

    <key>InputMethodServerControllerClass</key>
    <string>KarukanIMExtension.KarukanInputController</string>

    <!-- App Extension エントリポイント -->
    <key>NSExtension</key>
    <dict>
        <key>NSExtensionPrincipalClass</key>
        <string>KarukanIMExtension.KarukanInputController</string>
        <key>NSExtensionPointIdentifier</key>
        <string>com.apple.inputmethodkit</string>
    </dict>

    <!-- 入力モード定義 -->
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
                <key>tsInputModeAlternateIcons</key>
                <dict/>
            </dict>
        </dict>
    </dict>
</dict>
</plist>
```

> **Bundle Identifier の命名規則**: Extension の Bundle ID は、ホストアプリの Bundle ID を
> プレフィックスにした形式（`.KarukanIMExtension` サフィックス）にする必要がある。
> 誤った場合、システムが Extension をホストアプリと関連付けできない。

---

## 3. Bridging Header

`karukan-macos/include/karukan_macos.h` を Swift から使用するためのブリッジングヘッダーを作成する。

### `KarukanIMExtension/KarukanBridge.h`

```c
// KarukanBridge.h
// Swift Bridging Header — karukan-macos C FFI を Swift へ公開する

#ifndef KarukanBridge_h
#define KarukanBridge_h

// プロジェクトルートからの相対パス
#include "../../karukan-macos/include/karukan_macos.h"

#endif /* KarukanBridge_h */
```

> **パスの確認**: `KarukanIM.xcodeproj` が `macos/` 直下にある場合、
> `../../karukan-macos/include/karukan_macos.h` が正しいパスになる。
> Xcode の Build Settings で `HEADER_SEARCH_PATHS` に
> `$(SRCROOT)/../../karukan-macos/include` を追加することで絶対パス依存を避けられる。

または `HEADER_SEARCH_PATHS` を設定して相対インクルードにする方法：

```c
// KarukanBridge.h（HEADER_SEARCH_PATHS 設定時）
#include "karukan_macos.h"
```

**Build Settings**:

```text
HEADER_SEARCH_PATHS = $(SRCROOT)/../../karukan-macos/include
```

---

## 4. dylib 組み込み

### 4-1. Build Phase: Rust ビルド + dylib コピー

`KarukanIMExtension` ターゲットの **Build Phases** に Run Script を追加する。

**位置**: "Compile Sources" より**前**に配置する。

```bash
#!/usr/bin/env bash
# Build Phase: Rust dylib のビルドとコピー
#
# このスクリプトは karukan-macos をビルドし、
# libkarukan_macos.dylib を Extension の Frameworks ディレクトリに配置する。

set -euo pipefail

REPO_ROOT="${SRCROOT}/../../"
DYLIB_NAME="libkarukan_macos.dylib"

# ビルド設定に応じて Rust プロファイルを選択
if [ "${CONFIGURATION}" = "Release" ]; then
    RUST_PROFILE="release"
    CARGO_FLAGS="--release"
else
    RUST_PROFILE="debug"
    CARGO_FLAGS=""
fi

RUST_TARGET_DIR="${REPO_ROOT}target/${RUST_PROFILE}"
SRC="${RUST_TARGET_DIR}/${DYLIB_NAME}"
DEST="${BUILT_PRODUCTS_DIR}/${FRAMEWORKS_FOLDER_PATH}/${DYLIB_NAME}"

echo "▶ Building karukan-macos (profile: ${RUST_PROFILE})..."
cd "${REPO_ROOT}"
cargo build -p karukan-macos ${CARGO_FLAGS}

echo "▶ Copying ${DYLIB_NAME} to Frameworks..."
mkdir -p "${BUILT_PRODUCTS_DIR}/${FRAMEWORKS_FOLDER_PATH}"
cp "${SRC}" "${DEST}"

echo "▶ Fixing install name to @rpath/${DYLIB_NAME}..."
install_name_tool -id "@rpath/${DYLIB_NAME}" "${DEST}"

echo "▶ Verifying..."
otool -D "${DEST}"
# 期待出力: @rpath/libkarukan_macos.dylib

echo "✓ Done: ${DEST}"
```

**Input Files**（Xcode が変更検知に使用）:

```text
$(SRCROOT)/../../karukan-macos/src/session.rs
$(SRCROOT)/../../karukan-macos/src/ffi/mod.rs
$(SRCROOT)/../../Cargo.lock
```

**Output Files**:

```text
$(BUILT_PRODUCTS_DIR)/$(FRAMEWORKS_FOLDER_PATH)/libkarukan_macos.dylib
```

> **パフォーマンス注記**: `cargo build` は Rust のインクリメンタルビルドキャッシュを使用するため、
> ソースが変更されていない場合は高速（< 1秒）で完了する。
> Input Files / Output Files を正確に設定することで Xcode のビルドキャッシュも活用できる。

### 4-2. dylib の署名設定

Build Phase スクリプト実行後、Xcode は `Embed Frameworks` フェーズで dylib を
自動的にコード署名する（開発時は Sign to Run Locally）。

`KarukanIMExtension` ターゲットの **Build Phases > Embed Frameworks** に
`libkarukan_macos.dylib` を追加する:

1. **+** ボタン → **Add Files...**
2. `${BUILD_DIR}/Debug/KarukanIMExtension.appex/Contents/Frameworks/libkarukan_macos.dylib`
   を参照（初回ビルド後に追加可能）

または、Run Script から直接コードサインする（簡易版）:

```bash
# Run Script の末尾に追加（開発時のみ）
if [ "${CODE_SIGNING_ALLOWED}" = "YES" ]; then
    codesign --force --sign "${EXPANDED_CODE_SIGN_IDENTITY}" \
        --timestamp=none "${DEST}"
fi
```

---

## 5. `KarukanInputController.swift`

### 設計方針

- Swift にロジックを置かない — IMK コールバックの引数振り分けのみ行う
- Rust FFI の戻り値に基づいてクライアントの preedit/commit を更新する
- 初期化完了前のキー入力はスルーする（`initialized` フラグで管理）

### 完全実装

```swift
// KarukanIMExtension/KarukanInputController.swift
import Cocoa
import InputMethodKit

/// karukan IME の入力コントローラ。
///
/// IMKInputController の 1 インスタンスが 1 アプリケーションの入力コンテキストに対応する。
/// Rust 側の KarukanSession と 1:1 で紐付く。
final class KarukanInputController: IMKInputController {

    // -----------------------------------------------------------------------
    // MARK: - Properties
    // -----------------------------------------------------------------------

    /// Rust セッション（opaque ポインタ）。nil = 初期化失敗
    private var session: OpaquePointer?

    /// karukan_session_init 完了フラグ。
    /// バックグラウンドで init 後、メインスレッドで true に設定される。
    private var initialized: Bool = false

    // -----------------------------------------------------------------------
    // MARK: - Lifecycle
    // -----------------------------------------------------------------------

    override init!(server: IMKServer!, delegate: Any!, client: Any!) {
        super.init(server: server, delegate: delegate, client: client)

        // セッション生成（軽量・メインスレッド OK）
        guard let ptr = karukan_session_new() else {
            // karukan_session_new は失敗時 NULL を返す
            return
        }
        session = ptr

        // リソースロード（Phase 1: 辞書・学習キャッシュ。Phase 3: モデル追加）
        // バックグラウンドスレッドで呼ぶ（Phase 3 のモデルロードに備えた設計）。
        let capturedSession = ptr
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            _ = karukan_session_init(capturedSession)
            DispatchQueue.main.async {
                self?.initialized = true
            }
        }
    }

    deinit {
        guard let session else { return }
        // 学習キャッシュを保存してから解放
        karukan_session_free(session)
    }

    // -----------------------------------------------------------------------
    // MARK: - Key Event Handling
    // -----------------------------------------------------------------------

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        // 初期化完了前はすべてスルー（アプリに伝える）
        guard initialized, let session else { return false }

        // KeyDown のみ処理（KeyUp, FlagsChanged 等は無視）
        guard event.type == .keyDown else { return false }

        let flags = event.modifierFlags
            .intersection(.deviceIndependentFlagsMask)

        // Command / Option / Control 付きキーはスルー
        // （システムショートカットや他アプリの機能を妨げない）
        if flags.contains(.command) || flags.contains(.option) || flags.contains(.control) {
            return false
        }

        // 特殊キー → karukan_push_key
        if let key = KarukanMacOSKey.from(keyCode: event.keyCode) {
            let consumed = karukan_push_key(session, key.rawValue) != 0
            updateClientState(client: sender)
            return consumed
        }

        // 印字可能文字 → karukan_push_char
        guard let chars = event.characters,
              let scalar = chars.unicodeScalars.first,
              scalar.value >= 0x20 else {
            return false
        }

        let consumed = chars.withCString { ptr in
            karukan_push_char(session, ptr) != 0
        }
        updateClientState(client: sender)
        return consumed
    }

    // -----------------------------------------------------------------------
    // MARK: - Server Events
    // -----------------------------------------------------------------------

    override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        // 必要に応じて preedit を再描画
    }

    override func deactivateServer(_ sender: Any!) {
        guard let session else {
            super.deactivateServer(sender)
            return
        }
        // フォーカス喪失時: 未確定文字があればコミット
        if karukan_is_empty(session) == 0 {
            forceCommit(client: sender)
        }
        // 学習キャッシュを永続化
        karukan_save_learning(session)
        super.deactivateServer(sender)
    }

    override func commitComposition(_ sender: Any!) {
        // IMK が強制コミットを要求した場合
        forceCommit(client: sender)
        super.commitComposition(sender)
    }

    // -----------------------------------------------------------------------
    // MARK: - Private Helpers
    // -----------------------------------------------------------------------

    /// Rust 側の状態（preedit / commit）を IMK クライアントに反映する。
    ///
    /// karukan_push_* 呼び出し後に必ず呼ぶこと。
    private func updateClientState(client: Any?) {
        guard let session, let client = client as AnyObject? else { return }

        // コミットテキストがあれば先に挿入
        if karukan_has_commit(session) != 0 {
            let ptr = karukan_get_commit(session)
            let text = ptr.map { String(cString: $0) } ?? ""
            if !text.isEmpty {
                client.insertText?(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        }

        // preedit テキストを更新
        let preeditPtr = karukan_get_preedit(session)
        let preeditText = preeditPtr.map { String(cString: $0) } ?? ""
        let caretBytes = Int(karukan_get_preedit_caret(session))

        if preeditText.isEmpty {
            // preedit をクリア
            client.setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        } else {
            // カーソル位置を文字インデックスに変換（バイトオフセット → Swift String.Index）
            let cursorCharPos = preeditText.utf8
                .prefix(caretBytes).count  // 近似値（ASCII の場合は一致）
            let cursorIndex = preeditText.index(
                preeditText.startIndex,
                offsetBy: min(
                    preeditText.utf8.prefix(caretBytes).string.count,
                    preeditText.count
                )
            )
            let cursorCharIndex = preeditText.distance(
                from: preeditText.startIndex, to: cursorIndex)

            let attrStr = NSMutableAttributedString(string: preeditText)
            let fullRange = NSRange(preeditText.startIndex..., in: preeditText)
            attrStr.addAttribute(.underlineStyle,
                                 value: NSUnderlineStyle.single.rawValue,
                                 range: fullRange)

            client.setMarkedText?(
                attrStr,
                selectionRange: NSRange(location: cursorCharIndex, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }
    }

    /// 未確定テキストを強制コミットする。
    private func forceCommit(client: Any?) {
        guard let session else { return }
        guard let client = client as AnyObject? else { return }

        // preedit に残っているテキストを Return キーでコミット
        if karukan_is_empty(session) == 0 {
            _ = karukan_push_key(session, KarukanMacOSKey.returnKey.rawValue)

            if karukan_has_commit(session) != 0 {
                let ptr = karukan_get_commit(session)
                let text = ptr.map { String(cString: $0) } ?? ""
                if !text.isEmpty {
                    client.insertText?(
                        text,
                        replacementRange: NSRange(location: NSNotFound, length: 0)
                    )
                }
            }
            // preedit をクリア
            client.setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }
    }
}

// ---------------------------------------------------------------------------
// MARK: - macOS キーコード → KarukanKey マッピング
// ---------------------------------------------------------------------------

/// macOS の仮想キーコード（Carbon key code）と `KarukanKey` の対応。
///
/// 参照: Carbon/HIToolbox/Events.h, kVK_* 定数
private enum KarukanMacOSKey: UInt32 {
    case returnKey  = 1   // KARUKAN_KEY_RETURN
    case backspace  = 2   // KARUKAN_KEY_BACKSPACE
    case escape     = 3   // KARUKAN_KEY_ESCAPE
    case space      = 4   // KARUKAN_KEY_SPACE
    case leftArrow  = 5   // KARUKAN_KEY_LEFT
    case rightArrow = 6   // KARUKAN_KEY_RIGHT
    case upArrow    = 7   // KARUKAN_KEY_UP
    case downArrow  = 8   // KARUKAN_KEY_DOWN
    case tab        = 9   // KARUKAN_KEY_TAB

    /// macOS 仮想キーコード（UInt16）から KarukanMacOSKey に変換する。
    /// 対応するキーがない場合は nil を返す。
    static func from(keyCode: UInt16) -> KarukanMacOSKey? {
        switch keyCode {
        case 36: return .returnKey   // kVK_Return
        case 76: return .returnKey   // kVK_ANSI_KeypadEnter
        case 51: return .backspace   // kVK_Delete (Backspace)
        case 53: return .escape      // kVK_Escape
        case 49: return .space       // kVK_Space
        case 123: return .leftArrow  // kVK_LeftArrow
        case 124: return .rightArrow // kVK_RightArrow
        case 125: return .downArrow  // kVK_DownArrow
        case 126: return .upArrow    // kVK_UpArrow
        case 48: return .tab         // kVK_Tab
        default: return nil
        }
    }
}
```

> **`any AnyObject` の `setMarkedText?` / `insertText?` 呼び出しについて**:
> IMK の `sender` は `IMKTextInput` プロトコルに準拠するが、Swift 6 で直接キャストすると
> コンパイルエラーになる場合がある。`as AnyObject` にキャストして Optional メッセージ送信
> (`?.`) を使う方法が最も互換性が高い。
> `@objc optional` メソッドとして定義されているため、AnyObject 経由の呼び出しで
> セレクタが存在しない場合は安全に no-op になる。

### `updateClientState` の preedit カーソル位置変換について

`karukan_get_preedit_caret` が返すのは**バイトオフセット**（UTF-8）。
Swift の `setMarkedText` が要求するのは**文字数（Unicode スカラー数）**。
変換が必要なため、上記実装では UTF-8 バイト列の `prefix` から文字数を計算している。

ASCII ローマ字入力（Phase 2 のスコープ）では 1バイト = 1文字のため誤差なし。
ひらがな（3バイト/文字）が混在する場合も正しく変換される。

---

## 6. ビルドと実行

### 6-1. 初回ビルド手順

```bash
# 1. Rust dylib のビルド（Xcode Build Phase でも実行されるが、事前確認用）
cd /path/to/karukan
cargo build -p karukan-macos

# 2. Xcode でビルド
# Target: KarukanIMExtension を選択
# Destination: My Mac
# Cmd+B
```

### 6-2. 生成される App Bundle 構造の確認

ビルド後、以下を確認する:

```bash
# KarukanIM.app の構造確認
DERIVED_DATA=~/Library/Developer/Xcode/DerivedData
APP_PATH=$(find "${DERIVED_DATA}" -name "KarukanIM.app" -maxdepth 6 | head -1)

echo "=== App Bundle 構造 ==="
find "${APP_PATH}" -type f | sort

echo "=== dylib install name ==="
otool -D "${APP_PATH}/Contents/PlugIns/KarukanIMExtension.appex/Contents/Frameworks/libkarukan_macos.dylib"
# 期待: @rpath/libkarukan_macos.dylib

echo "=== Extension の rpath 設定 ==="
otool -l "${APP_PATH}/Contents/PlugIns/KarukanIMExtension.appex/Contents/MacOS/KarukanIMExtension" \
  | grep -A 2 "LC_RPATH"
# 期待: path @loader_path/../Frameworks
```

---

## 7. IME 登録と有効化

### 7-1. 初回インストール

macOS の Input Method を有効化するには、アプリを `/Library/Input Methods/` または
`~/Library/Input Methods/` に配置する必要がある。

```bash
# ユーザーローカルへのインストール（sudo 不要）
DERIVED_DATA=~/Library/Developer/Xcode/DerivedData
APP_PATH=$(find "${DERIVED_DATA}" -name "KarukanIM.app" -maxdepth 6 | head -1)

cp -R "${APP_PATH}" ~/Library/Input\ Methods/KarukanIM.app
```

### 7-2. IME 登録コマンド

```bash
# TIS（Text Input Sources）にIMEを登録
/System/Library/Frameworks/Carbon.framework/Versions/A/Support/AddressBook.app/../../../Versions/A/Resources/AddressBook.app/../../../Versions/A/Resources/RegisterInputSources.app/Contents/MacOS/RegisterInputSources \
    ~/Library/Input\ Methods/KarukanIM.app

# または macOS 13+ では:
defaults write ~/Library/Preferences/com.apple.HIToolbox.plist \
    AppleEnabledInputSources \
    -array-add '<dict><key>Bundle ID</key><string>com.example.karukan.KarukanIM.KarukanIMExtension</string><key>InputSourceKind</key><string>Non Roman Input Method</string></dict>'
```

> **推奨**: ターミナルからの手動登録は繁雑。開発中は以下のシェルスクリプトを `macos/scripts/install-dev.sh` として作成して使う。

### `macos/scripts/install-dev.sh`

```bash
#!/usr/bin/env bash
# 開発用 IME インストールスクリプト
# Usage: ./install-dev.sh [Debug|Release]

set -euo pipefail

CONFIG="${1:-Debug}"
DERIVED_DATA="${HOME}/Library/Developer/Xcode/DerivedData"
INSTALL_DIR="${HOME}/Library/Input Methods"

# ビルド成果物を検索
APP_PATH=$(find "${DERIVED_DATA}" \
    -name "KarukanIM.app" \
    -path "*/Build/Products/${CONFIG}/*" \
    -maxdepth 8 | head -1)

if [ -z "${APP_PATH}" ]; then
    echo "Error: KarukanIM.app not found. Build first with Xcode (${CONFIG})."
    exit 1
fi

echo "Found: ${APP_PATH}"

# 旧バージョンを削除
if [ -d "${INSTALL_DIR}/KarukanIM.app" ]; then
    echo "Removing old installation..."
    rm -rf "${INSTALL_DIR}/KarukanIM.app"
fi

# インストール
echo "Installing to ${INSTALL_DIR}..."
cp -R "${APP_PATH}" "${INSTALL_DIR}/KarukanIM.app"

# IME を再登録
echo "Registering input method..."
# killall InputMethodKit またはログアウト/ログインで反映
killall -9 InputMethodKit 2>/dev/null || true

echo "✓ Installed. Please:"
echo "  1. Open System Settings > Keyboard > Input Sources"
echo "  2. Add 'Karukan' from the list"
echo "  Note: May require logout/login on first install."
```

### 7-3. システム設定での有効化

1. **システム設定 > キーボード > 入力ソース**を開く
2. **+** ボタン → 日本語 → **Karukan** を選択して追加
3. 入力ソース切り替えショートカット（Control+Space 等）で切り替える

### 7-4. 初回インストール後の再起動

初回インストール時は TIS データベースへの登録のためにログアウト/ログインが必要な場合がある。
開発中の再インストールは通常不要。

---

## 8. デバッグ

### 8-1. os_log / Console.app

Extension はバックグラウンドプロセスとして動作するため、Xcode デバッガが直接アタッチしにくい。
`os_log` または `tracing` を使ってログを出力し、Console.app で確認する。

#### tracing ログの確認（Rust 側）

```bash
# Console.app を起動
open /System/Applications/Utilities/Console.app

# フィルタ: Process = "KarukanIMExtension"
# または subsystem = "karukan"
```

Rust 側の `tracing` は `stderr` に出力する設定になっている（`ffi/mod.rs` の `init_logging`）。
Extension のプロセスの stderr は Console.app の "Messages" に表示される。

ログレベルを上げる場合:

```bash
# launchctl setenv は Extension プロセスには伝わらない場合がある
# Install.sh の前に設定する必要がある
# 代替: 一時的に init_logging で "debug" にハードコード
```

#### Swift 側ログの追加

`KarukanInputController.swift` に以下を追加すると便利:

```swift
import OSLog

private let logger = Logger(subsystem: "com.example.karukan", category: "InputController")

// 使用例:
logger.debug("push_char: \(chars.debugDescription), consumed: \(consumed)")
logger.info("initialized: \(self.initialized)")
```

### 8-2. プロセスへのアタッチ

```bash
# Extension プロセスの PID を確認
pgrep -l KarukanIMExtension

# lldb でアタッチ（Xcode の "Debug > Attach to Process by PID or Name..." でも可）
sudo lldb -p $(pgrep KarukanIMExtension)
```

Xcode の **Debug > Attach to Process > KarukanIMExtension** を使う方法が最も簡単。
アタッチ後、Swift ブレークポイントが機能する。

### 8-3. よくある問題と対処

| 症状 | 原因候補 | 対処 |
|---|---|---|
| IME が入力ソースリストに現れない | Bundle ID のプレフィックスが一致しない | Extension の Bundle ID を `HostApp.BundleID + ".KarukanIMExtension"` に修正 |
| IME を選択してもキーを受け取らない | Extension が起動していない | Console.app でクラッシュログを確認 |
| dylib が見つからない | `@rpath` 未設定 / install name が絶対パス | `otool -D dylib` と `otool -l extension` の rpath を確認 |
| preedit が表示されない | `setMarkedText` の引数誤り | `sender as AnyObject` にキャストできているか確認 |
| `initialized` が常に false | `karukan_session_init` が失敗 | Console でエラーログを確認。戻り値 `-1` なら init 失敗 |
| テキストフィールドによってpreeditの挙動が異なる | アプリが TSM/IMK に対応していない | Safari、TextEdit で動作確認。Electron アプリ等は独自IMK実装 |

---

## 9. 動作確認手順

IME インストール・有効化後、以下の順番で動作確認する。

### 基本動作チェックリスト

```text
[x] 1. TextEdit を開き、Karukan 入力ソースに切り替える

[x] 2. "a" を押す
      期待: preedit に "あ"（下線付き）が表示される

[x] 3. "i" を押す
      期待: preedit が "あい" になる

[x] 4. Return を押す
      期待: "あい" がコミットされ、preedit が消える

[x] 5. "k" を押す
      期待: preedit に "k" が表示される（未確定ローマ字）

[x] 6. "a" を押す
      期待: preedit が "か" になる

[x] 7. "nnnichiha" と順に押す
      期待: preedit が "んにちは" になる（"konnnichiha" で "こんにちは"）

[x] 8. Backspace を押す
      期待: preedit の最後の文字が削除される

[x] 9. Backspace を連打して preedit を空にする
      期待: preedit が消える（下線も消える）

[x] 10. Escape を押す（preedit に何か入力した後）
       期待: preedit がキャンセルされてコミットなし

[ ] 11. フォーカスを他のアプリに移す（Command+Tab 等）
        期待: 未確定テキストがコミットされる（deactivateServer 動作確認）
```

---

## 10. 完了条件（Acceptance Criteria）

### Xcode ビルド

- [x] `cargo build -p karukan-macos` 成功後、Xcode で `Cmd+B` が成功する
- [x] `KarukanIM.app` の `PlugIns/KarukanIMExtension.appex/Contents/Frameworks/` に
      `libkarukan_macos.dylib` が存在する
- [x] `otool -D libkarukan_macos.dylib` の出力が `@rpath/libkarukan_macos.dylib`
- [x] `otool -l KarukanIMExtension` に `LC_RPATH: @loader_path/../Frameworks` が含まれる

### IME 動作

- [x] システム設定 > キーボード > 入力ソース に "Karukan" が表示される
- [x] TextEdit でローマ字入力 → ひらがなの preedit（下線付き）が表示される
- [x] Enter キーでひらがながコミットされる
- [x] Escape キーで入力がキャンセルされる
- [x] Backspace キーで1文字ずつ削除できる
- [x] "konnnichiha" → preedit に "こんにちは" が表示される
- [x] 未確定状態でフォーカスを失うとコミットされる
- [x] クラッシュなし（Console.app にクラッシュログが出ない）

---

## 11. Phase 3 への引き継ぎ事項

Phase 2 完了時に以下の状態であること:

1. **Space キーの動作**: 現在「全角スペースをひらがなとして preedit に追加」する。
   Phase 3 で `karukan_push_key(KARUKAN_KEY_SPACE)` が変換トリガーになるよう
   Rust 側で変更する。Swift 側のコードは変更不要。

2. **`karukan_session_init` の呼び出し**: Phase 2 から `DispatchQueue.global().async` で
   呼んでいるため、Phase 3 でモデルロードが追加されても Swift 側の変更は不要。

3. **候補ウィンドウの準備**: Phase 3 で `IMKCandidates` を使用するため、
   `KarukanInputController` に `candidates: IMKCandidates` プロパティを追加する予定。
   Phase 2 では不要だが、将来の追加箇所を `// TODO: Phase 3 - IMKCandidates` コメントで明示しておくと良い。

4. **Bundle Identifier のカスタマイズ**: `com.example.karukan` は仮の値。
   Phase 4 の公証・配布に向けて、開発者の Apple ID に紐付いた正式な Bundle ID に変更する。

5. **`karukan_get_candidate_count` / `karukan_get_candidate`**: Phase 3 で Rust 側に追加予定の関数。
   `karukan_macos.h` にはコメントアウトで宣言済み。Phase 3 で宣言を有効化する。

---

## 付録: macOS 仮想キーコード対応表

| キー名 | kVK 定数 | 10進数 | `KarukanMacOSKey` |
|---|---|---|---|
| Return | `kVK_Return` | 36 | `.returnKey` |
| Keypad Enter | `kVK_ANSI_KeypadEnter` | 76 | `.returnKey` |
| Delete (Backspace) | `kVK_Delete` | 51 | `.backspace` |
| Escape | `kVK_Escape` | 53 | `.escape` |
| Space | `kVK_Space` | 49 | `.space` |
| Left Arrow | `kVK_LeftArrow` | 123 | `.leftArrow` |
| Right Arrow | `kVK_RightArrow` | 124 | `.rightArrow` |
| Down Arrow | `kVK_DownArrow` | 125 | `.downArrow` |
| Up Arrow | `kVK_UpArrow` | 126 | `.upArrow` |
| Tab | `kVK_Tab` | 48 | `.tab` |

> 参照: `/System/Library/Frameworks/Carbon.framework/Headers/HIToolbox/Events.h`
