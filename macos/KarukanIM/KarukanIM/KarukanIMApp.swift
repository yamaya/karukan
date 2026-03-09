//
//  KarukanIMApp.swift
//  KarukanIM
//
//  ホストアプリ（LSUIElement）。
//  Input Method Extension のコンテナとして機能する。UI は持たない。
//

import SwiftUI
import InputMethodKit

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
