// SettingStore.swift
// KarukanIM — 設定値の共有ストア
//
// Preference Pane（サンドボックス外）と IME（サンドボックス内）の間で
// 設定値を共有するために UserDefaults(suiteName:) を使用する。

import Foundation

enum SettingStore {

    /// App Group suite name（Preference Pane と IME で共通）。
    static let suiteName = "com.example.inputmethod.KarukanIM"

    /// 共有 UserDefaults。suite が利用できない場合は .standard にフォールバック。
    static var defaults: UserDefaults {
        UserDefaults(suiteName: suiteName) ?? .standard
    }

    // MARK: - Keys

    static let liveConversionEnabledKey = "karukanLiveConversionEnabled"
    static let consonantDelaySecKey     = "karukanConsonantDelaySec"
    static let autoCommitMaxCharsKey    = "karukanAutoCommitMaxChars"

    // MARK: - Default values

    static func registerDefaults() {
        defaults.register(defaults: [
            liveConversionEnabledKey: true,
            consonantDelaySecKey: 0.1,
            autoCommitMaxCharsKey: 30,
        ])
    }
}
