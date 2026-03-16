// KarukanIMExtension.swift
// KarukanIMExtension
//
// IMKInputController サブクラス。
// Generic Extension テンプレートの ExtensionFoundation ボイラープレートを
// InputMethodKit ベースの実装に置き換えている。
// @main / AppExtension は使わない（IMK は NSExtensionPrincipalClass で読み込まれる）。

import Cocoa
import InputMethodKit
import OSLog

private let logger = Logger(subsystem: "io.github.yamaya.karukan", category: "InputController")

// ---------------------------------------------------------------------------
// MARK: - KarukanInputController
// ---------------------------------------------------------------------------

/// karukan IME の入力コントローラ。
///
/// IMKInputController の 1 インスタンスが 1 アプリケーションの
/// 入力コンテキストに対応する。Rust 側の KarukanSession と 1:1 で紐付く。
final class KarukanInputController: IMKInputController {

    // -----------------------------------------------------------------------
    // MARK: - Properties
    // -----------------------------------------------------------------------

    /// Rust セッション（opaque ポインタ）。nil = 生成失敗
    private var session: OpaquePointer?

    /// karukan_session_init 完了フラグ。
    /// バックグラウンドで init 後、メインスレッドで true に設定される。
    private var initialized: Bool = false

    /// setMarkedText で非空テキストをセットした状態かどうか。
    /// setMarkedText("") の呼び出しを「実際に marked text がある場合のみ」に制限するために使う。
    /// Empty 状態からの直接コミット（auto-commit）では setMarkedText("") を呼ぶと
    /// 直前の insertText がキャンセルされる app があるため。
    private var hasPreedit: Bool = false

    /// 初期化完了前に到着したキーイベントのバッファ。
    private var pendingEvents: [(event: NSEvent, sender: Any)] = []

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

        // リソースロード（辞書・学習キャッシュ・モデル）
        let capturedSession = ptr
        if karukan_is_prewarmed() != 0 {
            // プリウォーム済み: 同期実行（辞書・学習キャッシュの I/O のみ）
            let ret = karukan_session_init(capturedSession)
            logger.info("karukan_session_init (sync, prewarmed) returned: \(ret)")
            initialized = true
        } else {
            // プリウォーム未完了: バックグラウンドで初期化 + リプレイ
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in
                let ret = karukan_session_init(capturedSession)
                logger.info("karukan_session_init (async) returned: \(ret)")
                DispatchQueue.main.async {
                    guard let self else { return }
                    self.initialized = true
                    self.replayPendingEvents()
                }
            }
        }
    }

    deinit {
        guard let session else { return }
        // 学習キャッシュを保存してから解放
        karukan_session_free(session)
        logger.debug("session freed")
    }

    // -----------------------------------------------------------------------
    // MARK: - Key Event Handling
    // -----------------------------------------------------------------------

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let session else { return false }

        // 初期化完了前: keyDown イベントをバッファして consumed を返す。
        if !initialized {
            if event.type == .keyDown, let sender {
                pendingEvents.append((event: event, sender: sender))
                logger.debug("buffered keyDown (pending init): keyCode=\(event.keyCode)")
            }
            return true
        }

        // KeyDown のみ処理
        guard event.type == .keyDown else { return false }

        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)

        // Command / Option / Control 付きキーはスルー
        if flags.contains(.command) || flags.contains(.option) || flags.contains(.control) {
            return false
        }

        // 特殊キー → karukan_push_key
        if let key = KarukanMacOSKey.from(keyCode: event.keyCode) {
            let consumed = karukan_push_key(session, key.rawValue) != 0
            logger.debug("push_key(\(key.rawValue)) consumed=\(consumed)")
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
        // フォーカス喪失時: 未確定文字があればコミット
        if karukan_is_empty(session) == 0 {
            forceCommit(client: sender)
        }
        hasPreedit = false
        // 学習キャッシュを永続化
        karukan_save_learning(session)
        super.deactivateServer(sender)
    }

    override func commitComposition(_ sender: Any!) {
        // IMK が強制コミットを要求した場合
        logger.info("commitComposition")
        forceCommit(client: sender)
        super.commitComposition(sender)
    }

    // -----------------------------------------------------------------------
    // MARK: - Private Helpers
    // -----------------------------------------------------------------------

    /// 初期化完了前にバッファしたキーイベントをリプレイする。
    private func replayPendingEvents() {
        let events = pendingEvents
        pendingEvents.removeAll()
        logger.info("replaying \(events.count) buffered key events")
        for pending in events {
            _ = handle(pending.event, client: pending.sender)
        }
    }

    /// Rust 側の状態（preedit / commit）を IMK クライアントに反映する。
    ///
    /// karukan_push_* 呼び出し後に必ず呼ぶこと。
    private func updateClientState(client: Any?) {
        guard let session else { return }
        let c = client as AnyObject

        // コミットテキストがあれば先に挿入してから preedit を更新
        if karukan_has_commit(session) != 0 {
            let text = karukan_get_commit(session).map { String(cString: $0) } ?? ""
            if !text.isEmpty {
                logger.debug("insertText: '\(text)'")
                c.insertText?(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        }

        // preedit テキストを更新
        let preeditText = karukan_get_preedit(session).map { String(cString: $0) } ?? ""
        let caretBytes  = Int(karukan_get_preedit_caret(session))

        if preeditText.isEmpty {
            // hasPreedit のときのみ setMarkedText("") を呼ぶ。
            // marked text がない状態で呼ぶと直前の insertText がキャンセルされる app がある。
            if hasPreedit {
                c.setMarkedText?(
                    "",
                    selectionRange: NSRange(location: 0, length: 0),
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
                hasPreedit = false
            }
        } else {
            // バイトオフセット → 文字数インデックス変換
            // ASCII ローマ字入力（Phase 2）では 1 byte = 1 char なので誤差なし。
            // ひらがな（3 bytes/char）混在時も UTF-8 prefix で正確に変換できる。
            let cursorCharIndex = preeditText.utf8
                .prefix(caretBytes)
                .reduce(0) { acc, byte in
                    // UTF-8 継続バイト（0x80〜0xBF）以外をカウント
                    (byte & 0xC0) != 0x80 ? acc + 1 : acc
                }

            let attrStr = NSMutableAttributedString(string: preeditText)
            let fullRange = NSRange(preeditText.startIndex..., in: preeditText)
            attrStr.addAttribute(
                .underlineStyle,
                value: NSUnderlineStyle.single.rawValue,
                range: fullRange
            )

            hasPreedit = true
            logger.debug("setMarkedText: '\(preeditText)' caret=\(cursorCharIndex)")
            c.setMarkedText?(
                attrStr,
                selectionRange: NSRange(location: cursorCharIndex, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }
    }

    /// 未確定テキストを強制コミットする（フォーカス喪失時等）。
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
        // preedit をクリア
        c.setMarkedText?(
            "",
            selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
        hasPreedit = false
    }
}

// ---------------------------------------------------------------------------
// MARK: - macOS 仮想キーコード → KarukanKey マッピング
// ---------------------------------------------------------------------------

/// macOS の仮想キーコード（Carbon key code）と `KarukanKey` enum 値の対応。
///
/// 参照: Carbon/HIToolbox/Events.h の kVK_* 定数
private enum KarukanMacOSKey: UInt32 {
    case returnKey  = 1  // KARUKAN_KEY_RETURN
    case backspace  = 2  // KARUKAN_KEY_BACKSPACE
    case escape     = 3  // KARUKAN_KEY_ESCAPE
    case space      = 4  // KARUKAN_KEY_SPACE
    case leftArrow  = 5  // KARUKAN_KEY_LEFT
    case rightArrow = 6  // KARUKAN_KEY_RIGHT
    case upArrow    = 7  // KARUKAN_KEY_UP
    case downArrow  = 8  // KARUKAN_KEY_DOWN
    case tab        = 9  // KARUKAN_KEY_TAB

    /// macOS 仮想キーコード（UInt16）から変換する。対応なしは nil。
    static func from(keyCode: UInt16) -> KarukanMacOSKey? {
        switch keyCode {
        case 36:  return .returnKey   // kVK_Return
        case 76:  return .returnKey   // kVK_ANSI_KeypadEnter
        case 51:  return .backspace   // kVK_Delete (Backspace)
        case 53:  return .escape      // kVK_Escape
        case 49:  return .space       // kVK_Space
        case 123: return .leftArrow   // kVK_LeftArrow
        case 124: return .rightArrow  // kVK_RightArrow
        case 125: return .downArrow   // kVK_DownArrow
        case 126: return .upArrow     // kVK_UpArrow
        case 48:  return .tab         // kVK_Tab
        default:  return nil
        }
    }
}
