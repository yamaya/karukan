//
//  KarukanIMApp.swift
//  KarukanIM
//
//  ホストアプリ（LSUIElement）。
//  Input Method Extension のコンテナとして機能する。UI は持たない。
//

import SwiftUI
import InputMethodKit
import OSLog

private let logger = Logger(subsystem: "io.github.yamaya.karukan", category: "App")

@main
struct KarukanIMApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        // ウィンドウ不要。LSUIElement = YES により Dock 非表示。
        Settings {
            EmptyView()
        }
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    /// IMKit サーバー。strong reference を保持しないとすぐ解放される。
    var server: IMKServer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // KanaKanjiConverter をバックグラウンドでプリロードする。
        // これにより後続の karukan_session_init() が高速化される
        // （辞書・学習キャッシュの読み込みのみ）。
        DispatchQueue.global(qos: .userInitiated).async {
            let ret = karukan_prewarm()
            logger.info("karukan_prewarm returned: \(ret)")
        }

        // IMKServer を起動。InputMethodConnectionName と一致する名前を使う。
        server = IMKServer(
            name: "io.github.yamaya.inputmethod.KarukanIM_Connection",
            bundleIdentifier: Bundle.main.bundleIdentifier
        )
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ app: NSApplication) -> Bool {
        return false
    }
}
