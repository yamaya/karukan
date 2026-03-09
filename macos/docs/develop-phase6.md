# Phase 6: 設定アプリ（SwiftUI）

## Context
`.prefPane` による設定画面がシステム環境設定で表示されない問題を解決できなかったため、独立した SwiftUI 設定アプリに切り替える。入力メニューの「設定...」から起動する。

## 実装

### 1. 新規ターゲット追加: KarukanPreferences（SwiftUI App）

- `macos/KarukanIM/KarukanPreferences/` ディレクトリ作成
- ファイル:
    - `KarukanPreferencesApp.swift` — SwiftUI App エントリポイント（単一ウィンドウ）
    - `SettingsView.swift` — `@AppStorage` + suite UserDefaults で3設定を管理
- `project.pbxproj` に新ターゲット追加（macOS App, SwiftUI lifecycle）

### 2. SettingsView の設計

```text
┌─ Karukan 設定 ──────────────────────┐
│                                     │
│  ライブ変換          [Toggle ON/OFF]│
│                                     │
│  子音遅延           ──●────  0.10秒│
│                                     │
│  自動コミット閾値    [- 30 文字 +]  │
│                                     │
└─────────────────────────────────────┘
```

- `@AppStorage` + `UserDefaults(suiteName: "com.example.inputmethod.KarukanIM")` で既存の `SettingStore` と同じ suite を使用
- キー名: `karukanLiveConversionEnabled`, `karukanConsonantDelaySec`, `karukanAutoCommitMaxChars`

### 3. ホストアプリに埋め込み
- KarukanIM（ホストアプリ）の Build Phases に Copy Files Phase 追加
    - Destination: Resources
    - KarukanPreferences.app をコピー
- Target Dependency: KarukanIM → KarukanPreferences

### 4. 入力メニューから起動
- `KarukanInputController.swift` の `openPreferences(_:)` を変更:
    - appex から host app を逆引きして `Contents/Resources/KarukanPreferences.app` を `NSWorkspace.shared.open()` で起動

### 5. 旧 Preferences prefPane の削除
- Preferences ターゲットを pbxproj から削除
- Copy Preferences Pane ビルドフェーズを削除
- `Preferences/` ディレクトリの prefPane 関連ファイルを削除（PreferencesController.swift, XIB, Info.plist, Preferences.h, Preferences.m）
- KarukanIMExtension の Target Dependency から Preferences を削除

## 修正対象ファイル
- `macos/KarukanIM/KarukanIM.xcodeproj/project.pbxproj` — ターゲット追加・削除
- `macos/KarukanIM/KarukanIMExtension/KarukanInputController.swift` — `openPreferences` 変更
- 新規: `macos/KarukanIM/KarukanPreferences/KarukanPreferencesApp.swift`
- 新規: `macos/KarukanIM/KarukanPreferences/SettingsView.swift`
- 削除: `macos/KarukanIM/Preferences/` 配下

## 検証
1. ビルド成功
2. `~/Library/Input Methods/KarukanIM.app/Contents/Resources/KarukanPreferences.app` が存在
3. 入力メニューの「設定...」クリックで設定アプリが起動
4. トグル・スライダー・ステッパーが動作し、IME 側に即時反映
