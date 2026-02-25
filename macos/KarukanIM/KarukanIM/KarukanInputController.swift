// KarukanInputController.swift
// KarukanIM — IMKit サーバープロセス本体

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

    /// 候補パネル（IMKServer と 1:1 で生成）。
    private var candidatesPanel: IMKCandidates?

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

        // 候補パネルを生成（スクロールリスト形式）
        candidatesPanel = IMKCandidates(
            server: server,
            panelType: kIMKSingleColumnScrollingCandidatePanel
        )

        // リソースロード（辞書・学習キャッシュ・モデル）
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
            updateCandidatesPanel(sender: sender)
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
        updateCandidatesPanel(sender: sender)
        return consumed
    }

    // -----------------------------------------------------------------------
    // MARK: - IMKCandidates データソース
    // -----------------------------------------------------------------------

    /// Rust から候補を取得して IMKCandidates に渡す。
    override func candidates(_ sender: Any!) -> [Any]! {
        guard let session else { return [] }
        let count = Int(karukan_get_candidate_count(session))
        logger.debug("candidates() count=\(count)")
        return (0..<count).compactMap { idx in
            karukan_get_candidate(session, UInt32(idx)).map { String(cString: $0) }
        }
    }

    /// 候補ウィンドウでクリックまたは Return で選択された。
    override func candidateSelected(_ candidateString: NSAttributedString!) {
        guard let session else { return }

        let text = candidateString.string
        logger.debug("candidateSelected: '\(text)'")

        // 文字列からインデックスを逆引きして select_candidate を呼ぶ
        let count = Int(karukan_get_candidate_count(session))
        var idx: UInt32 = 0
        for i in 0..<count {
            if let ptr = karukan_get_candidate(session, UInt32(i)),
               String(cString: ptr) == text {
                idx = UInt32(i)
                break
            }
        }
        _ = karukan_select_candidate(session, idx)

        // コミットテキストをクライアントに送る
        if karukan_has_commit(session) != 0,
           let ptr = karukan_get_commit(session) {
            let committed = String(cString: ptr)
            if !committed.isEmpty {
                logger.debug("candidateSelected commit: '\(committed)'")
                (client() as AnyObject).insertText?(
                    committed,
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
            }
        }

        candidatesPanel?.hide()
        (client() as AnyObject).setMarkedText?(
            "",
            selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
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
        candidatesPanel?.hide()
        if karukan_is_empty(session) == 0 {
            forceCommit(client: sender)
        }
        karukan_save_learning(session)
        super.deactivateServer(sender)
    }

    override func commitComposition(_ sender: Any!) {
        logger.info("commitComposition")
        candidatesPanel?.hide()
        forceCommit(client: sender)
        super.commitComposition(sender)
    }

    // -----------------------------------------------------------------------
    // MARK: - Private Helpers
    // -----------------------------------------------------------------------

    /// 候補数に応じてパネルを表示 / 非表示する。
    /// Rust 側のカーソル位置に合わせてパネルの選択行も同期する。
    private func updateCandidatesPanel(sender: Any?) {
        guard let panel = candidatesPanel, let session else { return }
        let count = karukan_get_candidate_count(session)
        if count > 0 {
            panel.update()
            if !panel.isVisible() {
                panel.show()
            } else {
                // IMKCandidates:selectCandidate not working here in KarukanIM
                // Temporary workaounrd
                let cursor = karukan_get_candidate_cursor(session)
                for _ in 0..<Int(cursor) {
                    switch panel.panelType() {
                    case kIMKSingleColumnScrollingCandidatePanel:
                        panel.moveDown(self)
                        // TODO: Shiftキーが押されていたら`moveUp`するかぁ
                    case kIMKSingleRowSteppingCandidatePanel:
                        panel.moveRight(self)
                    default:
                        panel.moveDown(self)
                    }
                }
            }
        } else {
            panel.hide()
        }
    }

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
