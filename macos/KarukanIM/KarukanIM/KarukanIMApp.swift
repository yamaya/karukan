//
//  KarukanIMApp.swift
//  KarukanIM
//
//  ホストアプリ（LSUIElement）。
//  Input Method Extension のコンテナとして機能する。UI は持たない。
//

import SwiftUI

@main
struct KarukanIMApp: App {
    // AppDelegate で applicationShouldTerminateAfterLastWindowClosed を制御
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        // ウィンドウ不要。LSUIElement = YES により Dock 非表示。
        // Phase 4 で設定 UI を追加する場合は Settings { SettingsView() } を追加する。
        Settings {
            EmptyView()
        }
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationShouldTerminateAfterLastWindowClosed(_ app: NSApplication) -> Bool {
        // ウィンドウを閉じてもアプリを終了しない
        return false
    }
}
