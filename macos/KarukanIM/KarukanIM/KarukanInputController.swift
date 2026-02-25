// KarukanInputController.swift
// KarukanIM — IMKit サーバープロセス本体
//
// KarukanIM.app 自体が IMKit サーバー。Extension は使わない。
// NSPrincipalClass = "KarukanInputController" (ObjC 名) として Info.plist に登録する。

import Cocoa
import InputMethodKit
import OSLog

private let logger = Logger(subsystem: "com.example.karukan", category: "InputController")

// ---------------------------------------------------------------------------
// MARK: - KarukanInputController
// ---------------------------------------------------------------------------

/// karukan IME の入力コントローラ。
///
/// IMKInputController の 1 インスタンスが 1 アプリケーションの
/// 入力コンテキストに対応する。Rust 側の KarukanSession と 1:1 で紐付く。
@objc(KarukanInputController)
final class KarukanInputController: IMKInputController {

    // -----------------------------------------------------------------------
    // MARK: - Properties
    // -----------------------------------------------------------------------

    /// Rust セッション（opaque ポインタ）。nil = 生成失敗
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
            logger.error("karukan_session_new() returned NULL")
            return
        }
        session = ptr
        logger.debug("session created: \(String(describing: ptr))")

        // リソースロード（辞書・学習キャッシュ。Phase 3: モデル追加予定）
        let capturedSession = ptr
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let ret = karukan_session_init(capturedSession)
            logger.info("karukan_session_init returned: \(ret)")
            DispatchQueue.main.async {
                self?.initialized = true
            }
        }
    }

    deinit {
        guard let session else { return }
        karukan_session_free(session)
        logger.debug("session freed")
    }

    // -----------------------------------------------------------------------
    // MARK: - Key Event Handling
    // -----------------------------------------------------------------------

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard initialized, let session else { return false }
        guard event.type == .keyDown else { return false }

        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if flags.contains(.command) || flags.contains(.option) || flags.contains(.control) {
            return false
        }

        if let key = KarukanMacOSKey.from(keyCode: event.keyCode) {
            let consumed = karukan_push_key(session, key.rawValue) != 0
            logger.debug("push_key(\(key.rawValue)) consumed=\(consumed)")
            updateClientState(client: sender)
            return consumed
        }

        guard let chars = event.characters,
              let scalar = chars.unicodeScalars.first,
              scalar.value >= 0x20 else {
            return false
        }

        let consumed = chars.withCString { ptr in
            karukan_push_char(session, ptr) != 0
        }
        logger.debug("push_char('\(chars)') consumed=\(consumed)")
        updateClientState(client: sender)
        return consumed
    }

    // -----------------------------------------------------------------------
    // MARK: - Server Events
    // -----------------------------------------------------------------------

    override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        logger.info("activateServer")
    }

    override func deactivateServer(_ sender: Any!) {
        logger.info("deactivateServer")
        guard let session else {
            super.deactivateServer(sender)
            return
        }
        if karukan_is_empty(session) == 0 {
            forceCommit(client: sender)
        }
        karukan_save_learning(session)
        super.deactivateServer(sender)
    }

    override func commitComposition(_ sender: Any!) {
        logger.info("commitComposition")
        forceCommit(client: sender)
        super.commitComposition(sender)
    }

    // -----------------------------------------------------------------------
    // MARK: - Private Helpers
    // -----------------------------------------------------------------------

    private func updateClientState(client: Any?) {
        guard let session else { return }
        let c = client as AnyObject

        if karukan_has_commit(session) != 0 {
            let text = karukan_get_commit(session).map { String(cString: $0) } ?? ""
            if !text.isEmpty {
                logger.debug("insertText: '\(text)'")
                c.insertText?(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        }

        let preeditText = karukan_get_preedit(session).map { String(cString: $0) } ?? ""
        let caretBytes  = Int(karukan_get_preedit_caret(session))

        if preeditText.isEmpty {
            c.setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        } else {
            let cursorCharIndex = preeditText.utf8
                .prefix(caretBytes)
                .reduce(0) { acc, byte in
                    (byte & 0xC0) != 0x80 ? acc + 1 : acc
                }

            let attrStr = NSMutableAttributedString(string: preeditText)
            let fullRange = NSRange(preeditText.startIndex..., in: preeditText)
            attrStr.addAttribute(
                .underlineStyle,
                value: NSUnderlineStyle.single.rawValue,
                range: fullRange
            )

            logger.debug("setMarkedText: '\(preeditText)' caret=\(cursorCharIndex)")
            c.setMarkedText?(
                attrStr,
                selectionRange: NSRange(location: cursorCharIndex, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }
    }

    private func forceCommit(client: Any?) {
        guard let session else { return }
        guard karukan_is_empty(session) == 0 else { return }
        let c = client as AnyObject

        _ = karukan_push_key(session, KarukanMacOSKey.returnKey.rawValue)

        if karukan_has_commit(session) != 0 {
            let text = karukan_get_commit(session).map { String(cString: $0) } ?? ""
            if !text.isEmpty {
                logger.debug("forceCommit: '\(text)'")
                c.insertText?(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        }
        c.setMarkedText?(
            "",
            selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
    }
}

// ---------------------------------------------------------------------------
// MARK: - macOS 仮想キーコード → KarukanKey マッピング
// ---------------------------------------------------------------------------

private enum KarukanMacOSKey: UInt32 {
    case returnKey  = 1
    case backspace  = 2
    case escape     = 3
    case space      = 4
    case leftArrow  = 5
    case rightArrow = 6
    case upArrow    = 7
    case downArrow  = 8
    case tab        = 9

    static func from(keyCode: UInt16) -> KarukanMacOSKey? {
        switch keyCode {
        case 36:  return .returnKey
        case 76:  return .returnKey
        case 51:  return .backspace
        case 53:  return .escape
        case 49:  return .space
        case 123: return .leftArrow
        case 124: return .rightArrow
        case 125: return .downArrow
        case 126: return .upArrow
        case 48:  return .tab
        default:  return nil
        }
    }
}
