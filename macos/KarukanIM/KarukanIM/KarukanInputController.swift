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

    /// 直近の有効な入力クライアント。
    /// パネルクリック時は client() が無効になるため、
    /// handle(_:client:) / activateServer(_:) で更新して保持する。
    private var currentSender: Any?

    /// ライブ変換の世代カウンタ。
    /// push_char のたびにインクリメントし、stale なバックグラウンド推論結果を廃棄する。
    /// karukan-im ではデバウンスなしで同期実行するため、ここでもデバウンスは設けない。
    private var liveConversionGeneration: Int = 0

    /// 同時推論を 1 件に制限するセマフォ。
    /// 推論が終わっていなければ新規タスクはスキップする（スレッド爆発防止）。
    private let liveConversionSemaphore = DispatchSemaphore(value: 1)

    /// ライブ変換の自動コミット閾値（ひらがな文字数）。
    /// この文字数を超えた状態で推論が完了したら文節分割コミットを行い、preedit の肥大化を防ぐ。
    ///
    /// 実測値（M シリーズ Mac、2026-02-26）:
    ///   1-5 chars: ~30-45ms  /  6-10: ~35-55ms  /  11-15: ~45-72ms  /  16: 58ms
    /// 線形外挿: 30 chars ≒ 90ms — 十分許容範囲内。
    /// Intel Mac では 3-5 倍になる可能性があるため 30 で余裕を持たせている。
    /// 子音 pending 遅延表示用タイマー。
    /// タイマー発火前に次のキーが来ればキャンセルされ、ちらつきを防ぐ。
    private var consonantDelayTimer: Timer?

    /// 子音 pending 遅延秒数（0 で無効＝従来動作）。SettingStore から読み取る。
    private var consonantDelaySec: TimeInterval {
        let v = SettingStore.defaults.double(forKey: SettingStore.consonantDelaySecKey)
        return v > 0 ? v : 0.1
    }

    /// 自動コミット閾値（文字数）。SettingStore から読み取る。
    private var autoCommitMaxChars: Int {
        let v = SettingStore.defaults.integer(forKey: SettingStore.autoCommitMaxCharsKey)
        return v > 0 ? v : 30
    }

    /// ライブ変換の有効/無効フラグ。SettingStore に永続化される。
    /// Ctrl+Shift+L でトグル。デフォルトは有効。
    private var isLiveConversionEnabled: Bool {
        get { SettingStore.defaults.bool(forKey: SettingStore.liveConversionEnabledKey) }
        set { SettingStore.defaults.set(newValue, forKey: SettingStore.liveConversionEnabledKey) }
    }

    // -----------------------------------------------------------------------
    // MARK: - Lifecycle
    // -----------------------------------------------------------------------

    override init!(server: IMKServer!, delegate: Any!, client: Any!) {
        super.init(server: server, delegate: delegate, client: client)

        // UserDefaults のデフォルト値を登録（初回起動時のみ有効）
        SettingStore.registerDefaults()

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
        currentSender = sender
        // 候補パネル表示中はキーイベントをパネルに委譲する。
        // interpretKeyEvents は Up/Down しか動かないため、Space/Tab は moveDown/Up で代替する。
        if let panel = candidatesPanel, panel.isVisible(), event.type == .keyDown {
            switch event.keyCode {
            case 53: // Escape → 変換キャンセル（Rust に渡してひらがな preedit に戻す）
                guard let session else { return true }
                _ = karukan_push_key(session, KarukanMacOSKey.escape.rawValue)
                updateClientState(client: sender)
                panel.hide()
            case 51: // Backspace → 変換キャンセルのみ（文字削除なし）
                guard let session else { return true }
                _ = karukan_push_key(session, KarukanMacOSKey.escape.rawValue)
                updateClientState(client: sender)
                panel.hide()
            case 49: // Space / Shift-Space
                if event.modifierFlags.contains(.shift) {
                    panel.moveUp(nil)
                } else {
                    panel.moveDown(nil)
                }
            case 48: // Tab / Shift-Tab
                if event.modifierFlags.contains(.shift) {
                    panel.moveUp(nil)
                } else {
                    panel.moveDown(nil)
                }
            case 125: // Down → 次候補
                panel.moveDown(nil)
            case 126: // Up → 前候補
                panel.moveUp(nil)
            default: // Return など → パネルに任せる（candidateSelected が呼ばれる）
                panel.interpretKeyEvents([event])
            }
            return true
        }

        guard initialized, let session else { return false }
        guard event.type == .keyDown else { return false }

        // 子音遅延タイマーをキャンセル（次のキーが来たので即座に最新状態へ更新）
        consonantDelayTimer?.invalidate()
        consonantDelayTimer = nil

        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)

        // Ctrl+Shift+L: ライブ変換のオン/オフトグル（37 = kVK_ANSI_L）
        if event.keyCode == 37, flags == [.control, .shift] {
            isLiveConversionEnabled.toggle()
            logger.info("live conversion toggled: \(self.isLiveConversionEnabled ? "enabled" : "disabled")")
            if !isLiveConversionEnabled {
                // 無効化時: 表示中の live_candidate をクリアしてひらがな表示に戻す
                _ = karukan_push_key(session, KarukanMacOSKey.escape.rawValue)
                updateClientState(client: sender)
            }
            return true
        }

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

        // Ctrl+;: 半角英数確定（41 = kVK_ANSI_Semicolon）
        if event.keyCode == 41, flags == [.control] {
            _ = karukan_push_key(session, KarukanMacOSKey.convertAscii.rawValue)
            updateClientState(client: sender)
            candidatesPanel?.hide()
            return true
        }

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

        // 子音 pending なら preedit 更新を遅延してちらつきを防ぐ
        if consumed
            && consonantDelaySec > 0
            && karukan_is_consonant_pending(session) != 0
        {
            updateCandidatesPanel(sender: sender)
            consonantDelayTimer = Timer.scheduledTimer(
                withTimeInterval: consonantDelaySec,
                repeats: false
            ) { [weak self] _ in
                guard let self else { return }
                self.consonantDelayTimer = nil
                self.updateClientState(client: self.currentSender ?? self.client())
            }
            return consumed
        }

        updateClientState(client: sender)
        updateCandidatesPanel(sender: sender)
        // 文字入力後にライブ変換をトリガー（Composing 状態でなければ内部で無視される）
        if consumed && isLiveConversionEnabled {
            triggerLiveConversion(sender: sender)
        }
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

        // コミットテキストをクライアントに送る。
        // パネルクリック後は client() が無効になるため currentSender を優先する。
        let c = (currentSender ?? client()) as AnyObject
        if karukan_has_commit(session) != 0,
           let ptr = karukan_get_commit(session) {
            let committed = String(cString: ptr)
            if !committed.isEmpty {
                logger.debug("candidateSelected commit: '\(committed)'")
                c.insertText?(
                    committed,
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
            }
        }

        candidatesPanel?.hide()
        c.setMarkedText?(
            "",
            selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
    }

    /// パネル上の選択候補が変わったとき（矢印キー・クリックによるフォーカス移動）に呼ばれる。
    /// preedit を選択中の候補文字列で更新する。
    /// commit はここでは行わない（確定時は candidateSelected が呼ばれる）。
    override func candidateSelectionChanged(_ candidateString: NSAttributedString!) {
        guard let c = (currentSender as AnyObject?) ?? (self.client() as AnyObject?) else { return }

        let text = candidateString.string
        logger.debug("candidateSelectionChanged: '\(text)'")

        let attrStr = NSMutableAttributedString(string: text)
        attrStr.addAttribute(
            .underlineStyle,
            value: NSUnderlineStyle.single.rawValue,
            range: NSRange(text.startIndex..., in: text)
        )
        c.setMarkedText?(
            attrStr,
            selectionRange: NSRange(location: text.count, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
    }

    // -----------------------------------------------------------------------
    // MARK: - Server Events
    // -----------------------------------------------------------------------

    override func activateServer(_ sender: Any!) {
        super.activateServer(sender)
        currentSender = sender
        logger.info("activateServer")
    }

    override func deactivateServer(_ sender: Any!) {
        logger.info("deactivateServer")
        consonantDelayTimer?.invalidate()
        consonantDelayTimer = nil
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
    /// パネルの選択はパネル自身が管理し、candidateSelectionChanged で preedit に反映する。
    private func updateCandidatesPanel(sender: Any?) {
        guard let panel = candidatesPanel, let session else { return }
        let count = karukan_get_candidate_count(session)
        if count > 0 {
            panel.update()
            if !panel.isVisible() {
                panel.show()
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

        // Empty 状態に戻ったら generation をインクリメントして残存タスクを無効化する
        if karukan_is_empty(session) != 0 {
            liveConversionGeneration &+= 1
        }
    }

    /// バックグラウンドで推論を起動し、完了後にメインスレッドでライブ変換結果を適用する。
    ///
    /// - メインスレッドで現在のひらがなを取得（karukan_get_composing_hiragana）
    /// - バックグラウンドで Arc<KanaKanjiConverter> のみ使って変換（karukan_convert_top1）
    /// - generation が一致するときのみ結果を適用（stale な結果を廃棄）
    /// - 長文（> autoCommitMaxChars）の場合は文節境界で分割し先頭文節のみコミット、
    ///   残余ひらがなを karukan_set_composing_hiragana で次の Composing へ引き継ぐ
    ///
    /// karukan-im との対比: Linux 版は同期実行だが macOS はメインスレッドをブロックできないため非同期にする。
    private func triggerLiveConversion(sender: Any?) {
        guard let session else { return }

        // メインスレッドで現在のひらがなを取得
        var buf = [CChar](repeating: 0, count: 512)
        let len = karukan_get_composing_hiragana(session, &buf, buf.count)
        guard len > 0 else { return }
        let hiragana = String(cString: buf)

        // 世代をインクリメント（前の推論が完了しても世代が違えば適用されない）
        liveConversionGeneration &+= 1
        let gen = liveConversionGeneration

        logger.debug("triggerLiveConversion: '\(hiragana)' gen=\(gen)")

        // self を strong capture してセッションが解放されないようにする。
        // deinit は DispatchQueue.main で動くため、このクロージャが完了するまで
        // karukan_session_free は呼ばれない。
        DispatchQueue.global(qos: .userInitiated).async { [self] in
            // 前の推論がまだ走っていれば今回はスキップ（同時推論を 1 件に制限）。
            // timeout: .now() = 非ブロッキング tryWait。取れなければ即 return。
            guard liveConversionSemaphore.wait(timeout: .now()) == .success else {
                logger.debug("live: skipped (inference busy) gen=\(gen)")
                return
            }
            defer { liveConversionSemaphore.signal() }

            // Arc<KanaKanjiConverter> のみアクセス（Send+Sync）— 全体の変換
            let t0 = CFAbsoluteTimeGetCurrent()
            let resultPtr = karukan_convert_top1(session, hiragana)
            let fullMs = Int((CFAbsoluteTimeGetCurrent() - t0) * 1000)
            logger.info("live infer: \(hiragana.count)chars \(fullMs)ms gen=\(gen)")

            guard let resultPtr else {
                logger.debug("karukan_convert_top1: nil (gen=\(gen))")
                return
            }
            let candidate = String(cString: resultPtr)
            karukan_free_string(resultPtr)

            // ── 長文の場合: 文節境界で分割して先頭文節のみコミット ───────────────
            // autoCommitCandidate: メインスレッドで apply する変換テキスト
            // tailHiragana: コミット後に次の Composing として注入する残余ひらがな
            var autoCommitCandidate = candidate
            var tailHiragana = ""
            if hiragana.count > self.autoCommitMaxChars,
               let boundaryIdx = Self.findClauseBoundary(in: hiragana) {
                let headHiragana = String(hiragana[..<boundaryIdx])
                let tail = String(hiragana[boundaryIdx...])
                // 先頭文節のみを変換（短いので高速）
                let t1 = CFAbsoluteTimeGetCurrent()
                if let headPtr = karukan_convert_top1(session, headHiragana) {
                    let headMs = Int((CFAbsoluteTimeGetCurrent() - t1) * 1000)
                    autoCommitCandidate = String(cString: headPtr)
                    karukan_free_string(headPtr)
                    tailHiragana = tail
                    logger.info("live infer (head): \(headHiragana.count)chars \(headMs)ms")
                    logger.debug("clause split: head='\(autoCommitCandidate)' tail='\(tailHiragana)'")
                }
                // headPtr が nil の場合: autoCommitCandidate = candidate（全体コミット）のまま
            }

            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                // stale な結果は廃棄
                guard self.liveConversionGeneration == gen else {
                    logger.debug("live conversion discarded (stale gen=\(gen))")
                    return
                }
                logger.debug("apply_live_candidate: '\(autoCommitCandidate)' gen=\(gen)")
                if karukan_apply_live_candidate(session, autoCommitCandidate) != 0 {
                    if hiragana.count > self.autoCommitMaxChars {
                        // 変換済みテキストをコミット（live_candidate が Some(漢字) の状態で Return）
                        _ = karukan_push_key(session, KarukanMacOSKey.returnKey.rawValue)
                        if !tailHiragana.isEmpty {
                            // 残余ひらがなを新規 Composing として注入し、ライブ変換を再トリガー
                            _ = karukan_set_composing_hiragana(session, tailHiragana)
                            self.updateClientState(client: sender ?? self.client())
                            self.triggerLiveConversion(sender: sender)
                            return
                        }
                    }
                    self.updateClientState(client: sender ?? self.client())
                }
            }
        }
    }

    /// ひらがな文字列の最初の文節境界（助詞直後の位置）を返す。
    ///
    /// 先頭 minHead 文字は必ず先頭文節に含め、それ以降で最初に現れる
    /// 助詞（1文字または2文字）の直後を境界とする。
    /// 境界が見つからなければ nil を返す。
    ///
    /// 例: "わたしはがっこうへいきます" → "は" の直後（インデックス 4 文字目）
    private static func findClauseBoundary(in hiragana: String) -> String.Index? {
        let oneChar: Set<Character> = ["は", "が", "を", "に", "で", "へ", "と", "も"]
        let twoChar: Set<String>    = ["から", "まで", "より", "って", "けど", "ので",
                                       "のに", "には", "では", "とは"]
        let chars = Array(hiragana)
        let n = chars.count
        let minHead = 3  // 最低3文字は head に含める（短すぎる分割防止）

        var i = minHead
        while i < n {
            // 2文字助詞を優先チェック（"には" を "に" より先にマッチさせる）
            if i + 1 < n {
                let two = String([chars[i], chars[i + 1]])
                if twoChar.contains(two) {
                    return hiragana.index(hiragana.startIndex, offsetBy: i + 2)
                }
            }
            // 1文字助詞
            if oneChar.contains(chars[i]) {
                return hiragana.index(hiragana.startIndex, offsetBy: i + 1)
            }
            i += 1
        }
        return nil
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

    // -----------------------------------------------------------------------
    // MARK: - 入力メニュー（メニューバードロップダウン）
    // -----------------------------------------------------------------------

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

        // 子音遅延サブメニュー
        let delayItem = NSMenuItem(title: "子音遅延", action: nil, keyEquivalent: "")
        let delaySubmenu = NSMenu(title: "子音遅延")
        let currentDelay = consonantDelaySec
        for (label, value) in [
            ("なし", 0.0),
            ("0.05 秒", 0.05),
            ("0.10 秒", 0.10),
            ("0.15 秒", 0.15),
            ("0.20 秒", 0.20),
            ("0.30 秒", 0.30),
        ] {
            let item = NSMenuItem(
                title: label,
                action: #selector(setConsonantDelay(_:)),
                keyEquivalent: ""
            )
            item.tag = Int(value * 1000) // ms を整数で格納
            item.state = abs(currentDelay - value) < 0.001 ? .on : .off
            delaySubmenu.addItem(item)
        }
        delayItem.submenu = delaySubmenu
        menu.addItem(delayItem)

        // 自動コミット閾値サブメニュー
        let commitItem = NSMenuItem(title: "自動コミット閾値", action: nil, keyEquivalent: "")
        let commitSubmenu = NSMenu(title: "自動コミット閾値")
        let currentMax = autoCommitMaxChars
        for chars in [10, 20, 30, 40, 50] {
            let item = NSMenuItem(
                title: "\(chars) 文字",
                action: #selector(setAutoCommitMaxChars(_:)),
                keyEquivalent: ""
            )
            item.tag = chars
            item.state = currentMax == chars ? .on : .off
            commitSubmenu.addItem(item)
        }
        commitItem.submenu = commitSubmenu
        menu.addItem(commitItem)

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

    @objc func setConsonantDelay(_ sender: NSMenuItem) {
        let value = Double(sender.tag) / 1000.0
        SettingStore.defaults.set(value, forKey: SettingStore.consonantDelaySecKey)
        logger.info("consonant delay changed via menu: \(value)s")
    }

    @objc func setAutoCommitMaxChars(_ sender: NSMenuItem) {
        let value = sender.tag
        SettingStore.defaults.set(value, forKey: SettingStore.autoCommitMaxCharsKey)
        logger.info("auto commit max chars changed via menu: \(value)")
    }

    @objc func openPreferences(_ sender: Any) {
        // macOS 15+: キーボード設定に直接遷移
        if let url = URL(string: "x-apple.systempreferences:com.apple.Keyboard-Settings.extension") {
            NSWorkspace.shared.open(url)
        }
    }
}

// ---------------------------------------------------------------------------
// MARK: - macOS 仮想キーコード → KarukanKey マッピング
// ---------------------------------------------------------------------------

private enum KarukanMacOSKey: UInt32 {
    case returnKey       = 1
    case backspace       = 2
    case escape          = 3
    case space           = 4
    case leftArrow       = 5
    case rightArrow      = 6
    case upArrow         = 7
    case downArrow       = 8
    case tab             = 9
    case convertHiragana = 10
    case convertKatakana = 11
    case convertAscii    = 12

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
