# Phase 5: 設定画面・キーボードショートカット・インジケーターアイコン

> 前提: Phase 4（ライブ変換の洗練）完了・動作確認済み
> 完了条件: Preference Pane による設定 UI、標準キーボードショートカット、
> カスタムインジケーターアイコンが動作すること
>
> **実装状況**: T2 ✅ / T3 ✅ / T1 部分完了（SettingStore + entitlements 済み、Preference Pane ターゲット作成は手動）

---

## 概要

Phase 5 では IME としての完成度を日常利用レベルに引き上げる。以下の 3 項目を実装する。

| # | 項目 | 概要 |
|---|---|---|
| T1 | 設定画面（Preference Pane） | システム環境設定 > キーボード > Karukan の右ペインに設定 UI を表示 |
| T2 | キーボードショートカット・入力メニュー | Ctrl+J/K/; + メニューバーのドロップダウンメニュー |
| T3 | インジケーターアイコン | メニューバーのアイコンを「あ」等のカスタム画像に変更 |

---

## T1: 設定画面（Preference Pane）

### 背景

現在の設定（ライブ変換 on/off）は `UserDefaults.standard` に直接保存しており、
ユーザーが GUI で変更する手段がない。macOS 標準の入力メソッドはシステム環境設定 >
キーボードの右ペインに設定画面を持つ。

### 設計方針

`PreferencePanes.framework` を使い、`.prefPane` バンドルとして設定画面を作成する。
（参考: 「日本語入力を作るときに必要だった本」第8章）

### バンドル構造

```text
KarukanIM.app/
└── Contents/
    ├── Frameworks/
    │   └── libkarukan_macos.dylib
    ├── Resources/
    │   └── Preferences.prefPane/          ← 新規
    │       └── Contents/
    │           ├── MacOS/
    │           │   └── Preferences
    │           ├── Resources/
    │           │   └── Preferences.xib    ← 設定画面 UI
    │           └── Info.plist
    ├── PlugIns/
    │   └── KarukanIMExtension.appex/
    └── Info.plist
```

### Xcode プロジェクト変更

1. **新規ターゲット追加**: Preference Pane テンプレートで `Preferences` ターゲットを作成
   - Product Name: `Preferences`
   - Bundle Identifier: `com.example.inputmethod.KarukanIM.Preferences`
   - Product: `Preferences.prefPane`

2. **Target Dependencies**: `KarukanIM` ターゲットの Build Phases に `Preferences` を追加
   （Preferences が先にビルドされる）

3. **Copy Files Phase**: `KarukanIM` ターゲットに Copy Files Phase を追加
   - Destination: `Resources`
   - Files: `Preferences.prefPane`

4. **Runpath Search Paths**: Preferences ターゲットの Build Settings で
   `@loader_path/../../../../Frameworks` を追加（IME アプリの Frameworks を参照）

5. **Swift 利用設定**: Preferences ターゲットで Swift を有効化
   （テンプレートは Objective-C だが Swift で記述する）

### 設定項目

| 設定キー | 型 | デフォルト | UI 部品 | 説明 |
|---|---|---|---|---|
| `karukanLiveConversionEnabled` | Bool | `true` | Toggle/Switch | ライブ変換の有効/無効 |
| `karukanConsonantDelaySec` | Double | `0.1` | Slider (0.0-0.3) | 子音 pending 遅延秒数 |
| `karukanAutoCommitMaxChars` | Int | `30` | Stepper (10-50) | 自動コミット閾値（文字数） |

### 設定の共有（サンドボックス問題）

Preference Pane はサンドボックス**外**で実行される（システム環境設定のプロセス）。
IME はサンドボックス**内**で実行される。そのため `UserDefaults.standard` を直接共有できない。

#### 解決策: `UserDefaults(suiteName:)` + entitlements 例外

**Preference Pane 側** — suite name を指定して書き込む:

```swift
class SettingStore {
    static let suiteName = "com.example.inputmethod.KarukanIM"

    static var defaults: UserDefaults {
        UserDefaults(suiteName: suiteName) ?? .standard
    }
}
```

**IME 側（KarukanInputController）** — 同じ suite name で読み取る:

```swift
private var isLiveConversionEnabled: Bool {
    get { SettingStore.defaults.bool(forKey: "karukanLiveConversionEnabled") }
    set { SettingStore.defaults.set(newValue, forKey: "karukanLiveConversionEnabled") }
}
```

**entitlements に例外を追加**（`KarukanIM.entitlements`）:

```xml
<!-- Preference Pane の UserDefaults を読み取るために必要 -->
<key>com.apple.security.temporary-exception.shared-preference.read-only</key>
<string>com.example.inputmethod.KarukanIM</string>
```

> **注意**: `register(defaults:)` はプロセスごとのメモリ上のデフォルト値のため、
> Preference Pane と IME の両方で呼ぶ必要がある。

### Preference Pane の Swift コード概要

```swift
import PreferencePanes

class PreferencesController: NSPreferencePane {
    @IBOutlet weak var liveConversionToggle: NSSwitch!
    @IBOutlet weak var consonantDelaySlider: NSSlider!
    @IBOutlet weak var autoCommitStepper: NSStepper!
    @IBOutlet weak var autoCommitLabel: NSTextField!

    override func mainViewDidLoad() {
        let defaults = SettingStore.defaults
        defaults.register(defaults: [
            "karukanLiveConversionEnabled": true,
            "karukanConsonantDelaySec": 0.1,
            "karukanAutoCommitMaxChars": 30,
        ])

        liveConversionToggle.state = defaults.bool(forKey: "karukanLiveConversionEnabled") ? .on : .off
        consonantDelaySlider.doubleValue = defaults.double(forKey: "karukanConsonantDelaySec")
        autoCommitStepper.integerValue = defaults.integer(forKey: "karukanAutoCommitMaxChars")
        updateAutoCommitLabel()
    }

    @IBAction func liveConversionChanged(_ sender: NSSwitch) {
        SettingStore.defaults.set(sender.state == .on, forKey: "karukanLiveConversionEnabled")
    }

    @IBAction func consonantDelayChanged(_ sender: NSSlider) {
        SettingStore.defaults.set(sender.doubleValue, forKey: "karukanConsonantDelaySec")
    }

    @IBAction func autoCommitChanged(_ sender: NSStepper) {
        SettingStore.defaults.set(sender.integerValue, forKey: "karukanAutoCommitMaxChars")
        updateAutoCommitLabel()
    }

    private func updateAutoCommitLabel() {
        autoCommitLabel.stringValue = "\(autoCommitStepper.integerValue) 文字"
    }
}
```

### IME 側の変更

現在ハードコードされている値を `UserDefaults` から読むように変更する:

```swift
// Before:
private let consonantDelaySec: TimeInterval = 0.1
private static let kLiveConversionMaxChars = 30

// After:
private var consonantDelaySec: TimeInterval {
    let v = SettingStore.defaults.double(forKey: "karukanConsonantDelaySec")
    return v > 0 ? v : 0.1  // 未設定時のフォールバック
}
private var autoCommitMaxChars: Int {
    let v = SettingStore.defaults.integer(forKey: "karukanAutoCommitMaxChars")
    return v > 0 ? v : 30
}
```

### テスト要件

```text
[ ] システム環境設定 > キーボード > Karukan で設定画面が右ペインに表示される
[ ] ライブ変換トグルを切り替えると UserDefaults に保存される
[ ] IME 側で設定値が反映される（ライブ変換 off → ひらがなのみ表示）
[ ] IME プロセス再起動後も設定が保持される
[ ] 子音遅延スライダーの変更が即座に反映される
[ ] 自動コミット閾値の変更が反映される
```

---

## T2: キーボードショートカット・入力メニュー

### 背景

macOS 標準の日本語入力では以下のショートカットが使える。karukan でも対応したい。
また、メニューバーの入力メソッドアイコンをクリックした際のドロップダウンメニュー（入力メニュー）に
項目を追加し、ライブ変換の切り替えや設定画面へのアクセスを提供する。

### 対応するショートカット

| ショートカット | 動作 | macOS 標準 |
|---|---|---|
| Ctrl+J | 入力中のテキストをひらがなに変換して確定 | あり |
| Ctrl+K | 入力中のテキストをカタカナに変換して確定 | あり |
| Ctrl+Shift+L | ライブ変換の on/off トグル | karukan 独自（実装済み） |
| Ctrl+; | 入力中のテキストを半角英数に変換して確定 | あり |

### 設計方針

- Rust 側に新しいキーコードを追加する（`KARUKAN_KEY_CONVERT_HIRAGANA` 等）
- Swift 側は修飾キー + keyCode を検出して対応する `karukan_push_key` を呼ぶ
- Rust 側で `input_buf.text` を変換してコミットする

### Rust 側変更

#### `KarukanKey` enum に追加

```rust
// karukan-macos/src/ffi/mod.rs
KARUKAN_KEY_CONVERT_HIRAGANA  = 10,
KARUKAN_KEY_CONVERT_KATAKANA  = 11,
KARUKAN_KEY_CONVERT_ASCII     = 12,
```

#### `session.rs` — 変換コミット処理

```rust
fn do_convert_hiragana(&mut self) {
    // romaji フラッシュ → input_buf.text をそのままコミット
    self.flush_romaji();
    if self.input_buf.text.is_empty() { return; }

    let text = std::mem::take(&mut self.input_buf.text);
    self.live_candidate = None;
    self.input_buf.cursor_chars = 0;
    self.romaji.reset();
    self.state = SessionState::Empty;
    self.commit.text = CString::new(text).unwrap_or_default();
    self.commit.dirty = true;
    self.update_preedit("");
}

fn do_convert_katakana(&mut self) {
    self.flush_romaji();
    if self.input_buf.text.is_empty() { return; }

    let katakana = karukan_engine::kana::hiragana_to_katakana(&self.input_buf.text);
    self.live_candidate = None;
    self.input_buf.clear();
    self.romaji.reset();
    self.state = SessionState::Empty;
    self.commit.text = CString::new(katakana).unwrap_or_default();
    self.commit.dirty = true;
    self.update_preedit("");
}

fn do_convert_ascii(&mut self) {
    self.flush_romaji();
    if self.input_buf.text.is_empty() { return; }

    // ひらがな→ローマ字逆変換（最長一致）
    let romaji = self.hiragana_to_romaji(&self.input_buf.text);
    self.live_candidate = None;
    self.input_buf.clear();
    self.romaji.reset();
    self.state = SessionState::Empty;
    self.commit.text = CString::new(romaji).unwrap_or_default();
    self.commit.dirty = true;
    self.update_preedit("");
}
```

#### `session.rs` — ひらがな→ローマ字逆変換テーブル

`karukan-engine` の `rules.rs` と同じルール一覧から逆引きテーブルを構築する。
`karukan-engine` は変更禁止のため、`karukan-macos` 側に逆変換ロジックを持つ。

```rust
use std::collections::HashMap;
use once_cell::sync::Lazy;

/// ひらがな→ローマ字の逆引きテーブル。
/// karukan-engine の rules.rs と同一のマッピングを逆方向にしたもの。
/// 複数のローマ字表記がある場合は最も一般的なものを採用（例: "し" → "si" ではなく "shi"）。
static REVERSE_ROMAJI: Lazy<Vec<(&str, &str)>> = Lazy::new(|| {
    let mut table = vec![
        // 長い方を先に並べる（最長一致のため）
        ("きゃ", "kya"), ("きゅ", "kyu"), ("きょ", "kyo"),
        ("しゃ", "sha"), ("しゅ", "shu"), ("しょ", "sho"),
        ("ちゃ", "cha"), ("ちゅ", "chu"), ("ちょ", "cho"),
        // ... 拗音・特殊音を先に定義 ...
        // 単独かな
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
        ("っ", "xtu"),
        // ... 完全なテーブルは実装時に rules.rs から生成 ...
    ];
    // ひらがなの長い順にソート（最長一致）
    table.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    table
});

impl KarukanSession {
    fn hiragana_to_romaji(&self, hiragana: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = hiragana.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let remaining = &hiragana[chars[..i].iter().map(|c| c.len_utf8()).sum::<usize>()..];
            let mut matched = false;
            for &(kana, romaji) in REVERSE_ROMAJI.iter() {
                if remaining.starts_with(kana) {
                    result.push_str(romaji);
                    i += kana.chars().count();
                    matched = true;
                    break;
                }
            }
            if !matched {
                // テーブルにないかな（句読点等）はそのまま
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }
}
```

> **「ん」の扱い**: `nn` を採用（`n` 単独だと次の文字と結合する可能性があるため）。
> 厳密には後続文字によって `n` / `nn` を切り替えるべきだが、Phase 5 では `nn` 固定とする。

### Swift 側変更

`handle(_:client:)` の修飾キーチェック部分を拡張する。

> **注意**: 現在の実装では `flags.contains(.control)` で `return false` しているため、
> Ctrl+J/K/; のチェックは**この早期リターンより前に配置する**必要がある。
> Ctrl+Shift+L（既存）と同じ位置に並べる。

```swift
let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)

// Ctrl+J: ひらがな確定（38 = kVK_ANSI_J）
if event.keyCode == 38, flags == [.control] {
    _ = karukan_push_key(session, KarukanMacOSKey.convertHiragana.rawValue)
    updateClientState(client: sender)
    candidatesPanel?.hide()
    return true
}

// Ctrl+K: カタカナ確定（40 = kVK_ANSI_K）
if event.keyCode == 40, flags == [.control] {
    _ = karukan_push_key(session, KarukanMacOSKey.convertKatakana.rawValue)
    updateClientState(client: sender)
    candidatesPanel?.hide()
    return true
}

// Ctrl+Shift+L: ライブ変換トグル（37 = kVK_ANSI_L）— 既存

// Ctrl+;: 半角英数（41 = kVK_ANSI_Semicolon）
if event.keyCode == 41, flags == [.control] {
    _ = karukan_push_key(session, KarukanMacOSKey.convertAscii.rawValue)
    updateClientState(client: sender)
    candidatesPanel?.hide()
    return true
}
```

### `KarukanMacOSKey` enum に追加

```swift
case convertHiragana = 10
case convertKatakana = 11
case convertAscii    = 12
```

### テスト要件

```text
[ ] 「にほんご」入力中に Ctrl+J → 「にほんご」がひらがなのまま確定
[ ] 「にほんご」入力中に Ctrl+K → 「ニホンゴ」がカタカナで確定
[ ] ライブ変換中（「日本語」表示）に Ctrl+J → 「にほんご」がひらがなで確定
[ ] ライブ変換中（「日本語」表示）に Ctrl+K → 「ニホンゴ」がカタカナで確定
[ ] 「にほんご」入力中に Ctrl+; → 「nihonngo」が半角英数で確定
[ ] ライブ変換中（「日本語」表示）に Ctrl+; → 「nihonngo」が半角英数で確定
[ ] 候補パネル表示中にも Ctrl+J/K/; が動作する
[ ] 空の状態（Empty）で Ctrl+J/K/; → 何も起きない（consumed=false）
```

### 入力メニュー

メニューバーの IME アイコンをクリックした際に表示されるドロップダウンメニューに項目を追加する。
（参考: 「日本語入力を作るときに必要だった本」第7章）

#### メニュー項目

| 項目 | 動作 | 備考 |
|---|---|---|
| ✓ ライブ変換 | on/off トグル（チェックマーク付き） | Ctrl+Shift+L と同じ動作 |
| 設定... | Preference Pane を開く | T1 実装後に有効化 |

#### Swift 側変更

`IMKInputController.menu()` をオーバーライドして `NSMenu` を返す:

```swift
override func menu() -> NSMenu! {
    let menu = NSMenu(title: "Karukan")

    // ライブ変換トグル
    let liveItem = NSMenuItem(
        title: "ライブ変換",
        action: #selector(toggleLiveConversion(_:)),
        keyEquivalent: ""
    )
    liveItem.state = isLiveConversionEnabled ? .on : .off
    menu.addItem(liveItem)

    menu.addItem(.separator())

    // 設定画面を開く
    let prefItem = NSMenuItem(
        title: "設定...",
        action: #selector(openPreferences(_:)),
        keyEquivalent: ""
    )
    menu.addItem(prefItem)

    return menu
}

@objc func toggleLiveConversion(_ sender: Any) {
    isLiveConversionEnabled.toggle()
    logger.info("live conversion toggled via menu: \(self.isLiveConversionEnabled ? "enabled" : "disabled")")
    if !isLiveConversionEnabled, let session {
        _ = karukan_push_key(session, KarukanMacOSKey.escape.rawValue)
        updateClientState(client: currentSender ?? client())
    }
}

@objc func openPreferences(_ sender: Any) {
    // Preference Pane を含むシステム環境設定を開く
    // macOS 15+: キーボード設定に直接遷移
    if let url = URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension") {
        NSWorkspace.shared.open(url)
    }
}
```

> **`menu()` の呼び出しタイミング**: メニューを開くたびに呼ばれるため、
> `isLiveConversionEnabled` のチェックマーク状態は常に最新になる。

#### テスト要件（入力メニュー）

```text
[ ] メニューバーの Karukan アイコンクリックで「ライブ変換」「設定...」が表示される
[ ] 「ライブ変換」クリックでチェックマークが切り替わり、動作が反映される
[ ] 「設定...」クリックでシステム環境設定のキーボード設定が開く
```

---

## T3: インジケーターアイコン

### 背景

現在メニューバーには Xcode デフォルトのアイコンが表示されている。
macOS 標準の日本語入力では「あ」「ア」「A」のようなアイコンでモードを示す。
karukan でも「あ」のカスタムアイコンを表示したい。

### 設計方針

`tsInputModeMenuIconFileKey` を使って入力モードごとのアイコンを指定する。
アイコンは `.tiff` または `.pdf` 形式で Extension の Resources に配置する。

### アイコン仕様

| ファイル名 | 表示 | サイズ | 用途 |
|---|---|---|---|
| `hiragana.pdf` | 「あ」 | 16x16 pt（@2x 対応） | ひらがなモード |

> 現状は入力モードが `hiragana` のみ。将来カタカナモード等を追加する際に
> `katakana.pdf`（「ア」）等を追加する。

### Info.plist 変更（Extension）

`ComponentInputModeDict` の入力モード定義にアイコンキーを追加する:

```xml
<key>com.example.inputmethod.karukan.hiragana</key>
<dict>
    <key>TISInputSourceIsASCIICapable</key>
    <false/>
    <key>TISInputSourceType</key>
    <string>com.apple.input-method.Roman</string>
    <key>TsInputMethodCharacterRepertoireKey</key>
    <array>
        <string>Latn</string>
        <string>Hira</string>
        <string>Kana</string>
    </array>
    <!-- アイコンファイル名（拡張子なし、Resources/ からの相対パス） -->
    <key>tsInputModeMenuIconFileKey</key>
    <string>hiragana</string>
</dict>
```

### アイコン作成

macOS メニューバーアイコンは **テンプレートイメージ** として扱われる。
黒色のシルエットを PDF ベクターで作成し、`hiragana.pdf` として保存する。

- サイズ: 16x16 pt（Retina 対応のためベクター推奨）
- 色: 黒一色（システムがダークモード/ライトモードに応じて色を調整）
- フォント: 太めのゴシック体で「あ」を描画

### ファイル配置

```text
macos/KarukanIM/KarukanIMExtension/
├── Resources/
│   └── hiragana.pdf       ← 新規
├── Info.plist             ← tsInputModeMenuIconFileKey を追加
└── ...
```

Xcode の KarukanIMExtension ターゲットの Bundle Resources に `hiragana.pdf` を追加する。

### テスト要件

```text
[ ] メニューバーに「あ」アイコンが表示される
[ ] ダークモード/ライトモードでアイコンが正しく表示される
[ ] Retina ディスプレイで鮮明に表示される
[ ] システム環境設定 > キーボード の入力ソース一覧にもアイコンが反映される
```

---

## 実装順序

```text
T2（ショートカット）→ T3（アイコン）→ T1（設定画面）

理由:
- T2 は Rust + Swift の小規模変更で完結。依存が少なく最初に着手しやすい
- T3 はアイコン作成 + Info.plist 変更のみ。コード変更が最小限
- T1 は Xcode ターゲット追加・サンドボックス設定・UI 構築と最も規模が大きい。
  T2/T3 で動作確認した上で最後に着手する
```

---

## アーキテクチャ上の注意事項

### Preference Pane と IME の通信

Preference Pane と IME は別プロセスで動作するため、設定変更の通知が必要。
`UserDefaults` は書き込み即座に永続化されるが、IME 側が読み取るタイミングは
次のキー入力時（`handle(_:client:)` が呼ばれたとき）。

リアルタイム通知が必要な場合は `DistributedNotificationCenter` を使う:

```swift
// Preference Pane 側: 設定変更時に通知
DistributedNotificationCenter.default().post(
    name: Notification.Name("com.example.karukan.settingsChanged"),
    object: nil
)

// IME 側: 通知を受けて設定を再読み込み
DistributedNotificationCenter.default().addObserver(
    self,
    selector: #selector(reloadSettings),
    name: Notification.Name("com.example.karukan.settingsChanged"),
    object: nil
)
```

> Phase 5 では `DistributedNotificationCenter` は必須ではない。
> 設定変更後に次のキー入力で反映される遅延（< 数百ms）は許容範囲。
> 将来的に即時反映が必要になった場合に導入する。

### SettingStore の共通化

`SettingStore` クラスは Preference Pane と IME の両方で使う。
ファイルを共有するため、Xcode の Target Membership を両ターゲットに設定する。

---

## 既知リスク

| # | リスク | 深刻度 | 対処 |
|---|---|---|---|
| R1 | PreferencePanes.framework が deprecated になる可能性 | 中 | macOS 15 では動作確認済み。代替が出た場合は移行 |
| R2 | サンドボックス例外 entitlements が App Store 審査で拒否される | 低 | 当面は Developer ID 署名で配布。App Store は Phase 6 以降で検討 |
| R3 | Ctrl+J/K/; がアプリ側のショートカットと競合 | 中 | IME が先にキーイベントを受け取るため基本的に問題ないが、特定アプリで競合する場合は設定で無効化できるようにする（将来） |
| R4 | テンプレートイメージのアイコンが一部の macOS テーマで見えにくい | 低 | Apple のガイドラインに従い黒色シルエットで作成。実機で確認 |
| R5 | ひらがな→ローマ字逆変換で「ん」の扱いが不完全 | 低 | Phase 5 では `nn` 固定。後続文字による `n`/`nn` 切り替えは将来改善 |

---

## テスト要件まとめ

```text
T1: 設定画面
  [ ] Preference Pane がシステム環境設定に表示される
  [ ] 設定値が UserDefaults(suiteName:) に保存される
  [ ] IME 側で設定値が読み取れる
  [ ] サンドボックス内から shared-preference が読める

T2: キーボードショートカット・入力メニュー
  [ ] Ctrl+J でひらがな確定
  [ ] Ctrl+K でカタカナ確定
  [ ] Ctrl+; で半角英数確定
  [ ] 候補パネル表示中でも動作する
  [ ] 入力メニューに「ライブ変換」「設定...」が表示される
  [ ] 「ライブ変換」メニューでトグルが動作する

T3: インジケーターアイコン
  [ ] メニューバーに「あ」アイコンが表示される
  [ ] ダーク/ライトモード両対応

全体
  [ ] cargo build -p karukan-macos がエラーなく成功する
  [ ] Xcode ビルドが成功する（3 ターゲット: KarukanIM, KarukanIMExtension, Preferences）
  [ ] 高速タイピング中にクラッシュしない
```

---

## 完了条件（Acceptance Criteria）

- [ ] システム環境設定 > キーボード > Karukan に設定画面が表示され、ライブ変換 on/off を切り替えられる
- [ ] Ctrl+J でひらがな確定、Ctrl+K でカタカナ確定、Ctrl+; で半角英数確定が動作する
- [ ] メニューバーに「あ」のカスタムアイコンが表示される
- [ ] `cargo build -p karukan-macos --release` がエラーなく成功する
- [ ] Xcode Archive ビルドが成功する

---

## 実装済みファイル一覧

### T2: キーボードショートカット・入力メニュー ✅

| ファイル | 変更内容 |
|---|---|
| `karukan-macos/src/session.rs` | `KarukanKey` に ConvertHiragana/Katakana/Ascii 追加、`do_convert_*` 3 メソッド、逆変換テーブル `REVERSE_ROMAJI`、テスト 16 件追加 |
| `karukan-macos/include/karukan_macos.h` | `KARUKAN_KEY_CONVERT_HIRAGANA/KATAKANA/ASCII` 定数追加 |
| `KarukanInputController.swift` | Ctrl+J/K/; ハンドラ、`menu()` オーバーライド、`toggleLiveConversion`/`openPreferences` |

### T3: インジケーターアイコン ✅

| ファイル | 変更内容 |
|---|---|
| `KarukanIMExtension/Resources/hiragana.pdf` | 「あ」16×16pt テンプレートアイコン（新規） |
| `KarukanIMExtension/Info.plist` | `tsInputModeMenuIconFileKey` 追加 |
| `KarukanIM/Info.plist` | `tsInputModeMenuIconFileKey` 追加 |

### T1: 設定画面（基盤） ✅ / Xcode ターゲット作成 🔧手動

| ファイル | 変更内容 |
|---|---|
| `KarukanIM/SettingStore.swift` | `UserDefaults(suiteName:)` 共有ストア（新規） |
| `KarukanInputController.swift` | `SettingStore` 経由に移行（`consonantDelaySec`, `autoCommitMaxChars`, `isLiveConversionEnabled`） |
| `KarukanIM.entitlements` | `shared-preference.read-only` 追加 |
| `Preferences/PreferencesController.swift` | Preference Pane ソース（新規、ターゲット作成手順コメント付き） |

---

## Phase 6 への引き継ぎ候補

1. **Preference Pane ターゲット作成** — `PreferencesController.swift` のコメントに手順記載済み。XIB UI 構築を含む
2. **Universal Binary・公証・配布** — develop-plan.md の元 Phase 4 内容
3. **設定項目の拡充** — キーバインドカスタマイズ、フォント設定等
4. **「ん」の逆変換改善** — 後続文字による `n`/`nn` 切り替え（現在は `nn` 固定）
