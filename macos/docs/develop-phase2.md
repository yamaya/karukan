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

- `target/release/libkarukan_macos.dylib` が存在すること
- `karukan-macos/include/karukan_macos.h` が存在すること
- `nm -D` で `_karukan_` プレフィックスのシンボルが T セクションに 12 個存在すること
- `otool -L` で依存が macOS システムライブラリのみであること（`/usr/lib/libSystem.B.dylib`、`Metal.framework` 等）

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

- `LSUIElement` = `true` — Dock に表示しない。入力メソッドはバックグラウンドで動作する。
- `NSPrincipalClass` = `NSApplication`

#### AppDelegate.swift

- `applicationDidFinishLaunching` では何もしない（ホストアプリ本体は UI 不要。将来 Phase 4 で設定ウィンドウを追加する）
- `applicationShouldTerminateAfterLastWindowClosed` は `false` を返す

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

以下のキーを設定する。

- `CFBundleIdentifier` = `com.example.karukan.KarukanIM.KarukanIMExtension`
- `CFBundleName` = `Karukan`
- `CFBundleDisplayName` = `Karukan`
- `NSPrincipalClass` = `KarukanIMExtension.KarukanInputController`
- `InputMethodConnectionName` = `KarukanIM`（IMK 接続名。Info.plist の値と一致させる）
- `InputMethodServerControllerClass` = `KarukanIMExtension.KarukanInputController`
- `NSExtension` — App Extension エントリポイント:
  - `NSExtensionPrincipalClass` = `KarukanIMExtension.KarukanInputController`
  - `NSExtensionPointIdentifier` = `com.apple.inputmethodkit`
- `ComponentInputModeDict` > `tsInputModeListKey` > `com.example.inputmethod.karukan.hiragana`:
  - `TISInputSourceIsASCIICapable` = `false`
  - `TISInputSourceType` = `com.apple.input-method.Roman`
  - `tsInputModeAlternateIcons` = （空辞書）

> **Bundle Identifier の命名規則**: Extension の Bundle ID は、ホストアプリの Bundle ID を
> プレフィックスにした形式（`.KarukanIMExtension` サフィックス）にする必要がある。
> 誤った場合、システムが Extension をホストアプリと関連付けできない。

---

## 3. Bridging Header

`karukan-macos/include/karukan_macos.h` を Swift から使用するためのブリッジングヘッダーを作成する。

### `KarukanIMExtension/KarukanBridge.h`

`KarukanBridge.h` は `karukan_macos.h` をインクルードするだけのシンプルなヘッダー。

- プロジェクトルートからの相対パス `../../karukan-macos/include/karukan_macos.h` でインクルードする
- または Build Settings の `HEADER_SEARCH_PATHS` に `$(SRCROOT)/../../karukan-macos/include` を追加し、ファイル名のみでインクルードする方法もある（絶対パス依存を避けられる）

> **パスの確認**: `KarukanIM.xcodeproj` が `macos/` 直下にある場合、
> `../../karukan-macos/include/karukan_macos.h` が正しいパスになる。

---

## 4. dylib 組み込み

### 4-1. Build Phase: Rust ビルド + dylib コピー

`KarukanIMExtension` ターゲットの **Build Phases** に Run Script を追加する。

**位置**: "Compile Sources" より**前**に配置する。

スクリプトが行う処理:

1. `CONFIGURATION` 変数を参照し、Release なら `--release` フラグ付きで `cargo build -p karukan-macos` を実行する（Debug なら省略）
2. ビルド後の `libkarukan_macos.dylib` を `$(BUILT_PRODUCTS_DIR)/$(FRAMEWORKS_FOLDER_PATH)/` へコピーする
3. `install_name_tool -id "@rpath/libkarukan_macos.dylib"` で install name を `@rpath` 相対に書き換える
4. `otool -D` で install name が `@rpath/libkarukan_macos.dylib` になっていることを検証する

**Input Files**（Xcode が変更検知に使用）:

- `$(SRCROOT)/../../karukan-macos/src/session.rs`
- `$(SRCROOT)/../../karukan-macos/src/ffi/mod.rs`
- `$(SRCROOT)/../../Cargo.lock`

**Output Files**:

- `$(BUILT_PRODUCTS_DIR)/$(FRAMEWORKS_FOLDER_PATH)/libkarukan_macos.dylib`

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

開発中は Run Script の末尾で `codesign --force --sign` を使って直接署名する方法も利用できる。

---

## 5. `KarukanInputController.swift`

### 設計方針

- Swift にロジックを置かない — IMK コールバックの引数振り分けのみ行う
- Rust FFI の戻り値に基づいてクライアントの preedit/commit を更新する
- 初期化完了前のキー入力はスルーする（`initialized` フラグで管理）

### クラス構成

`KarukanInputController` は `IMKInputController` を継承し、以下の責務を持つ。

**プロパティ**:

- `session: OpaquePointer?` — Rust セッション（opaque ポインタ）。`nil` は初期化失敗を意味する
- `initialized: Bool` — `karukan_session_init` 完了フラグ

**ライフサイクル**:

- `init(server:delegate:client:)` で `karukan_session_new()` を呼んでセッションを生成する（軽量・メインスレッド OK）
- リソースロード（辞書・学習キャッシュ）は `DispatchQueue.global(qos: .userInitiated).async` でバックグラウンド実行し、完了後にメインスレッドで `initialized = true` にセットする（Phase 3 のモデルロードに備えた設計）
- `deinit` で `karukan_session_free` を呼ぶ（学習キャッシュの保存も内部で実行される）

**キーイベント処理** (`handle(_:client:)`):

- `initialized` が `false` の場合、またはセッションが `nil` の場合は `false` を返してスルーする
- `KeyDown` のみ処理し、`Command` / `Option` / `Control` 修飾キー付きはスルーする
- 特殊キー（Return / Backspace / Escape / Space / 矢印 / Tab）は macOS 仮想キーコードから `KarukanMacOSKey` に変換して `karukan_push_key` を呼ぶ
- 印字可能文字（U+0020 以上）は `karukan_push_char` を呼ぶ
- 両パスとも最後に `updateClientState(client:)` を呼んで IMK クライアントに状態を反映する

**サーバーイベント**:

- `deactivateServer`: 未確定文字があれば `forceCommit`、その後 `karukan_save_learning` で学習キャッシュを永続化する
- `commitComposition`: IMK が強制コミットを要求した場合に `forceCommit` を呼ぶ

**プライベートヘルパー**:

- `updateClientState(client:)` — `karukan_has_commit` でコミット文字列を確認して `insertText` し、`karukan_get_preedit` で preedit テキストを取得して `setMarkedText` に渡す。カーソル位置は `karukan_get_preedit_caret` が返す UTF-8 バイトオフセットを文字数インデックスに変換する
- `forceCommit(client:)` — `karukan_push_key(KARUKAN_KEY_RETURN)` で未確定テキストをコミットし、preedit をクリアする

**macOS キーコードマッピング** (`KarukanMacOSKey`):

macOS の仮想キーコード（Carbon key code）と `KarukanKey` の対応は付録を参照。
`from(keyCode:)` が変換を担当し、対応するキーがない場合は `nil` を返す。

### `updateClientState` の preedit カーソル位置変換について

`karukan_get_preedit_caret` が返すのは**バイトオフセット**（UTF-8）。
Swift の `setMarkedText` が要求するのは**文字数（Unicode スカラー数）**。
変換が必要なため、UTF-8 バイト列の `prefix` から文字数を計算している。

ASCII ローマ字入力（Phase 2 のスコープ）では 1バイト = 1文字のため誤差なし。
ひらがな（3バイト/文字）が混在する場合も正しく変換される。

> **`any AnyObject` の `setMarkedText?` / `insertText?` 呼び出しについて**:
> IMK の `sender` は `IMKTextInput` プロトコルに準拠するが、Swift 6 で直接キャストすると
> コンパイルエラーになる場合がある。`as AnyObject` にキャストして Optional メッセージ送信
> (`?.`) を使う方法が最も互換性が高い。
> `@objc optional` メソッドとして定義されているため、AnyObject 経由の呼び出しで
> セレクタが存在しない場合は安全に no-op になる。

---

## 6. ビルドと実行

### 6-1. 初回ビルド手順

1. Rust dylib を事前確認目的でビルドする: `cargo build -p karukan-macos`（Xcode Build Phase でも自動実行される）
2. Xcode で `KarukanIMExtension` ターゲットを選択し、Destination を "My Mac" にして `Cmd+B` を実行する

### 6-2. 生成される App Bundle 構造の確認

ビルド後に以下を確認する:

- `KarukanIM.app/Contents/PlugIns/KarukanIMExtension.appex/Contents/Frameworks/libkarukan_macos.dylib` が存在すること
- `otool -D libkarukan_macos.dylib` の出力が `@rpath/libkarukan_macos.dylib` であること
- `otool -l KarukanIMExtension` に `LC_RPATH: path @loader_path/../Frameworks` が含まれること

---

## 7. IME 登録と有効化

### 7-1. 初回インストール

macOS の Input Method を有効化するには、アプリを `~/Library/Input Methods/` に配置する必要がある。

手順:
1. DerivedData から `KarukanIM.app` を検索する
2. `~/Library/Input Methods/KarukanIM.app` としてコピーする

### 7-2. IME 登録コマンド

TIS（Text Input Sources）への登録は以下の方法のいずれかで行う:

- `RegisterInputSources` ツール（`Carbon.framework` に付属）を使って登録する
- macOS 13+ では `defaults write` で `AppleEnabledInputSources` に Bundle ID を追加する

> **推奨**: ターミナルからの手動登録は繁雑。開発中は `macos/scripts/install-dev.sh` を用意して使う。

### `macos/scripts/install-dev.sh` の処理内容

開発用インストールスクリプトが行う処理:

1. `Debug` または `Release`（引数で指定）のビルド成果物を DerivedData から検索する
2. `~/Library/Input Methods/KarukanIM.app` が既に存在すれば削除する
3. 新しい `KarukanIM.app` をコピーする
4. `killall -9 InputMethodKit` で InputMethodKit を再起動する（初回は反映されない場合あり）

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

**Rust 側のログ確認**:

- Console.app を起動し、Process フィルタを `KarukanIMExtension`、または subsystem フィルタを `karukan` に設定する
- Rust 側の `tracing` は `stderr` に出力する（`ffi/mod.rs` の `init_logging`）。Extension の stderr は Console.app の "Messages" に表示される
- ログレベルを変更するには `init_logging` 内でハードコードする（`launchctl setenv` は Extension プロセスに伝わらない場合がある）

**Swift 側のログ**:

- `import OSLog` して `Logger(subsystem:category:)` を使用する
- subsystem は `com.example.karukan`、category は `InputController` 等を指定する

### 8-2. プロセスへのアタッチ

- `pgrep -l KarukanIMExtension` で Extension プロセスの PID を確認する
- Xcode の **Debug > Attach to Process > KarukanIMExtension** を使うのが最も簡単
- アタッチ後、Swift ブレークポイントが機能する

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

- [x] 1. TextEdit を開き、Karukan 入力ソースに切り替える
- [x] 2. "a" を押す → preedit に "あ"（下線付き）が表示される
- [x] 3. "i" を押す → preedit が "あい" になる
- [x] 4. Return を押す → "あい" がコミットされ、preedit が消える
- [x] 5. "k" を押す → preedit に "k" が表示される（未確定ローマ字）
- [x] 6. "a" を押す → preedit が "か" になる
- [x] 7. "nnnichiha" と順に押す → preedit が "んにちは" になる（"konnnichiha" で "こんにちは"）
- [x] 8. Backspace を押す → preedit の最後の文字が削除される
- [x] 9. Backspace を連打して preedit を空にする → preedit が消える（下線も消える）
- [x] 10. Escape を押す（preedit に何か入力した後）→ preedit がキャンセルされてコミットなし
- [ ] 11. フォーカスを他のアプリに移す（Command+Tab 等）→ 未確定テキストがコミットされる（deactivateServer 動作確認）

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
