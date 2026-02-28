// PreferencesController.swift
// Karukan Preference Pane
//
// システム環境設定 > キーボード > Karukan の右ペインに表示される設定画面。
// PreferencePanes.framework を使用し、.prefPane バンドルとして KarukanIM.app に埋め込む。
//
// Xcode でのターゲット作成手順:
// 1. File > New > Target > Preference Pane を選択
//    - Product Name: Preferences
//    - Bundle Identifier: com.example.inputmethod.KarukanIM.Preferences
// 2. KarukanIM ターゲットの Build Phases > Target Dependencies に Preferences を追加
//    （Preferences が先にビルドされるようにする）
// 3. KarukanIM ターゲットの Build Phases > Copy Files Phase を追加
//    - Destination: Resources
//    - Files: Preferences.prefPane
// 4. このファイルと SettingStore.swift を Preferences ターゲットに追加
// 5. Preferences ターゲットの Build Settings > Runpath Search Paths に
//    @loader_path/../../../../Frameworks を追加

import PreferencePanes

class PreferencesController: NSPreferencePane {

    // MARK: - Outlets (Interface Builder で接続)

    @IBOutlet weak var liveConversionToggle: NSSwitch!
    @IBOutlet weak var consonantDelaySlider: NSSlider!
    @IBOutlet weak var consonantDelayLabel: NSTextField!
    @IBOutlet weak var autoCommitStepper: NSStepper!
    @IBOutlet weak var autoCommitLabel: NSTextField!

    // MARK: - Lifecycle

    override func mainViewDidLoad() {
        SettingStore.registerDefaults()

        let defaults = SettingStore.defaults

        // ライブ変換トグル
        liveConversionToggle.state = defaults.bool(forKey: SettingStore.liveConversionEnabledKey) ? .on : .off

        // 子音遅延スライダー
        consonantDelaySlider.minValue = 0.0
        consonantDelaySlider.maxValue = 0.3
        consonantDelaySlider.doubleValue = defaults.double(forKey: SettingStore.consonantDelaySecKey)
        updateConsonantDelayLabel()

        // 自動コミット閾値ステッパー
        autoCommitStepper.minValue = 10
        autoCommitStepper.maxValue = 50
        autoCommitStepper.increment = 5
        autoCommitStepper.integerValue = defaults.integer(forKey: SettingStore.autoCommitMaxCharsKey)
        updateAutoCommitLabel()
    }

    // MARK: - Actions

    @IBAction func liveConversionChanged(_ sender: NSSwitch) {
        SettingStore.defaults.set(sender.state == .on, forKey: SettingStore.liveConversionEnabledKey)
    }

    @IBAction func consonantDelayChanged(_ sender: NSSlider) {
        SettingStore.defaults.set(sender.doubleValue, forKey: SettingStore.consonantDelaySecKey)
        updateConsonantDelayLabel()
    }

    @IBAction func autoCommitChanged(_ sender: NSStepper) {
        SettingStore.defaults.set(sender.integerValue, forKey: SettingStore.autoCommitMaxCharsKey)
        updateAutoCommitLabel()
    }

    // MARK: - Private

    private func updateConsonantDelayLabel() {
        consonantDelayLabel.stringValue = String(format: "%.2f 秒", consonantDelaySlider.doubleValue)
    }

    private func updateAutoCommitLabel() {
        autoCommitLabel.stringValue = "\(autoCommitStepper.integerValue) 文字"
    }
}
