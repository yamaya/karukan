// KarukanInputController.swift
// KarukanIM — IMKit サーバープロセス本体

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
@objc(KarukanInputController)
final class KarukanInputController: IMKInputController, NSMenuItemValidation {

    // -----------------------------------------------------------------------
    // MARK: - Properties
    // -----------------------------------------------------------------------

    /// Rust セッション（opaque ポインタ）。nil = 生成失敗
    private var session: OpaquePointer?

    /// karukan_session_init 完了フラグ。
    /// バックグラウンドで init 後、メインスレッドで true に設定される。
    private var initialized: Bool = false

    /// 候補パネル（IMKServer と 1:1 で共有）。
    /// static に保持して個別コントローラの dealloc で解放されないようにする。
    /// IMKit の _IMKServerLegacy が内部で unretained 参照を持つため、
    /// インスタンスごとに解放するとサーバ側の参照がダングリングし
    /// deactivateServer → isVisible で EXC_BAD_ACCESS を起こす。
    private static var sharedCandidatesPanel: IMKCandidates?
    private var candidatesPanel: IMKCandidates? { Self.sharedCandidatesPanel }

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

    /// セマフォビジーでスキップされた場合の再トリガーフラグ。
    /// 推論完了後にメインスレッドで再度 triggerLiveConversion を呼ぶ。
    private var liveConversionNeedsRetrigger: Bool = false

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
        SettingStore.defaults.double(forKey: SettingStore.consonantDelaySecKey)
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

    /// 入力メニュー（一度だけ構築して保持する）。
    /// IMKInputController.menu() は呼ばれるたびに再構築すると target の weak 参照が
    /// 切れるリスクがあるため、azooKey 方式でプロパティとして保持する。
    private var appMenu: NSMenu!

    /// preedit が存在するか（Composing / Conversion 状態）。
    /// validateMenuItem で変換ショートカット項目のグレーアウト制御に使う。
    private var isComposing = false

    /// setMarkedText で非空テキストをセットした状態かどうか。
    /// deactivateServer 内で setMarkedText("") の呼び出しを
    /// 「実際に marked text がある場合のみ」に制限するために使う。
    /// （Empty 状態からの直接コミット直後に setMarkedText("") を呼ぶと
    ///  直前の insertText がキャンセルされる app があるため）
    private var hasPreedit: Bool = false

    /// 候補パネルの現在のカーソル位置（0-based）。
    /// candidateSelectionChanged で更新し、末尾 Space 時の先頭ラップ判定に使う。
    private var candidateCursor: Int = 0

    /// 初期化完了前に到着したキーイベントのバッファ。
    /// `initialized` が true になった時点でリプレイされる。
    private var pendingEvents: [(event: NSEvent, sender: Any)] = []

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

        // 候補パネルを生成（スクロールリスト形式・サーバと 1:1 で共有）
        if Self.sharedCandidatesPanel == nil {
            Self.sharedCandidatesPanel = IMKCandidates(
                server: server,
                panelType: kIMKSingleColumnScrollingCandidatePanel
            )
        }
        // スリープ前に候補パネルを閉じる。
        // スリープでウィンドウサーバーとの接続が切れると、復帰後の
        // deactivateServer で IMKit がパネルに isVisible を送った際に
        // 既に無効なメモリへアクセスしてクラッシュするのを防ぐ。
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(handleWillSleep),
            name: NSWorkspace.willSleepNotification,
            object: nil
        )

        // 入力メニューを一度だけ構築（target の weak 参照が切れないよう self が生きている間に固定）
        setupMenu()

        // 起動時の設定値ログ（永続化確認用）
        logger.info("settings on init — liveConversion=\(self.isLiveConversionEnabled) consonantDelay=\(self.consonantDelaySec)s autoCommitMax=\(self.autoCommitMaxChars)")

        // リソースロード（辞書・学習キャッシュ・モデル）
        let capturedSession = ptr
        if karukan_is_prewarmed() != 0 {
            // プリウォーム済み: モデルは Arc::clone のみ。辞書・学習キャッシュの I/O だけなので
            // メインスレッドで同期実行しても十分高速（数十 ms）。
            // これにより初期化完了前のキー取りこぼしを完全に回避できる。
            let ret = karukan_session_init(capturedSession)
            logger.info("karukan_session_init (sync, prewarmed) returned: \(ret)")
            initialized = true
        } else {
            // プリウォーム未完了: バックグラウンドで初期化し、完了後にバッファ済みイベントをリプレイする。
            // [self] strong capture: karukan_session_init 完了前に deinit/karukan_session_free が
            // 走るとフリーしたポインタにアクセスして落ちるため、init 完了まで self を生かし続ける。
            DispatchQueue.global(qos: .userInitiated).async { [self] in
                let ret = karukan_session_init(capturedSession)
                logger.info("karukan_session_init (async) returned: \(ret)")
                DispatchQueue.main.async { [weak self] in
                    guard let self else { return }
                    self.initialized = true
                    self.replayPendingEvents()
                }
            }
        }
    }

    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
        guard let session else { return }
        karukan_session_free(session)
        logger.debug("session freed")
    }

    /// スリープ前に候補パネルを閉じる。
    /// スリープでウィンドウサーバーとの接続が切れる前にパネルを非表示にしておく。
    @objc private func handleWillSleep(_ notification: Notification) {
        logger.info("willSleep — dismissing candidates panel")
        candidatesPanel?.hide()
    }

    // -----------------------------------------------------------------------
    // MARK: - Key Event Handling
    // -----------------------------------------------------------------------

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        currentSender = sender

        // JIS かな (104) / 英数 (102): keyDown・flagsChanged・keyUp すべて消費する。
        // 候補パネル・initialized ガードより前で処理しないとアプリに漏れて空白が挿入される。
        if event.keyCode == 104 || event.keyCode == 102 {
            return true
        }

        // 候補パネル表示中はキーイベントをパネルに委譲する。
        // interpretKeyEvents は Up/Down しか動かないため、Space/Tab は moveDown/Up で代替する。
        if let panel = candidatesPanel, panel.isVisible(), event.type == .keyDown {
            // Ctrl+J/K/;: パネル表示中でも変換確定を優先する
            let panelFlags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            if panelFlags == [.control], let session,
               let key: KarukanMacOSKey = ({
                   switch event.keyCode {
                   case 38: return .convertHiragana
                   case 40: return .convertKatakana
                   case 41: return .convertAscii
                   case 34: return .shrinkSegment   // Ctrl+I
                   case 31: return .extendSegment   // Ctrl+O
                   default: return nil
                   }
               })() {
                _ = karukan_push_key(session, key.rawValue)
                updateClientState(client: sender)
                panel.hide()
                return true
            }

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
            case 123: // Left / Shift+Left → 候補パネルを閉じて前の文節へ / 文節縮小
                guard let session else { return true }
                if event.modifierFlags.contains(.shift) {
                    _ = karukan_push_key(session, KarukanMacOSKey.shrinkSegment.rawValue)
                } else {
                    _ = karukan_push_key(session, KarukanMacOSKey.leftArrow.rawValue)
                }
                updateClientState(client: sender)
                panel.hide()
            case 124: // Right / Shift+Right → 候補パネルを閉じて次の文節へ / 文節拡大
                guard let session else { return true }
                if event.modifierFlags.contains(.shift) {
                    _ = karukan_push_key(session, KarukanMacOSKey.extendSegment.rawValue)
                } else {
                    _ = karukan_push_key(session, KarukanMacOSKey.rightArrow.rawValue)
                }
                updateClientState(client: sender)
                panel.hide()
            case 49: // Space / Shift-Space
                if event.modifierFlags.contains(.shift) {
                    panel.moveUp(nil)
                } else {
                    let total = session.map { Int(karukan_get_candidate_count($0)) } ?? 0
                    if candidateCursor >= total - 1 {
                        panel.update() // 最終項目 → 先頭へラップ
                    } else {
                        panel.moveDown(nil)
                    }
                }
            case 48: // Tab / Shift-Tab
                if event.modifierFlags.contains(.shift) {
                    panel.moveUp(nil)
                } else {
                    let total = session.map { Int(karukan_get_candidate_count($0)) } ?? 0
                    if candidateCursor >= total - 1 {
                        panel.update() // 最終項目 → 先頭へラップ
                    } else {
                        panel.moveDown(nil)
                    }
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

        guard let session else { return false }

        // 初期化完了前: keyDown イベントをバッファして consumed を返す。
        // 初期化完了後にリプレイされる。
        if !initialized {
            if event.type == .keyDown, let sender {
                pendingEvents.append((event: event, sender: sender))
                logger.debug("buffered keyDown (pending init): keyCode=\(event.keyCode)")
            }
            return true
        }

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

        // Ctrl+I / Shift+Left: 文節縮小（BunsetsuConversion 状態のときのみ消費）
        // 34 = kVK_ANSI_I, 123 = kVK_LeftArrow
        if (event.keyCode == 34 && flags == [.control])
            || (event.keyCode == 123 && flags.contains(.shift) && !flags.contains(.command))
        {
            if karukan_get_segment_count(session) > 0 {
                _ = karukan_push_key(session, KarukanMacOSKey.shrinkSegment.rawValue)
                updateClientState(client: sender)
                candidatesPanel?.hide()
                return true
            }
            return false
        }

        // Ctrl+O / Shift+Right: 文節拡大（BunsetsuConversion 状態のときのみ消費）
        // 31 = kVK_ANSI_O, 124 = kVK_RightArrow
        if (event.keyCode == 31 && flags == [.control])
            || (event.keyCode == 124 && flags.contains(.shift) && !flags.contains(.command))
        {
            if karukan_get_segment_count(session) > 0 {
                _ = karukan_push_key(session, KarukanMacOSKey.extendSegment.rawValue)
                updateClientState(client: sender)
                candidatesPanel?.hide()
                return true
            }
            return false
        }

        // Option+/: 全角スラッシュ（44 = kVK_ANSI_Slash）
        if event.keyCode == 44, flags == [.option] {
            if karukan_is_empty(session) == 0 {
                forceCommit(client: sender)
            }
            let c = sender as AnyObject
            c.insertText?("／", replacementRange: NSRange(location: NSNotFound, length: 0))
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
        logger.debug("push_char('\(chars, privacy: .public)') keyCode=\(event.keyCode) flags=\(event.modifierFlags.rawValue) consumed=\(consumed)")

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

        // preedit と候補パネルを即時更新（push_key パスと同じ）
        updateClientState(client: sender)
        updateCandidatesPanel(sender: sender)

        // Rust が消費しなかった記号を全角に変換して挿入
        if !consumed, let fullWidth = Self.fullWidthMap[chars] {
            if karukan_is_empty(session) == 0 {
                forceCommit(client: sender)
            }
            let c = sender as AnyObject
            c.insertText?(fullWidth, replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }

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

        // パネルクリック後は client() が無効になるため currentSender を優先する。
        let c = (currentSender ?? client()) as AnyObject

        if karukan_has_commit(session) != 0 {
            // 通常コミット（全文節一括確定など）
            let committed = karukan_get_commit(session).map { String(cString: $0) } ?? ""
            if !committed.isEmpty {
                logger.debug("candidateSelected commit: '\(committed)'")
                c.insertText?(
                    committed,
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
            }
            candidatesPanel?.hide()
            c.setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        } else {
            // 文節候補の確定: コミットなし、preedit を更新して文節ナビへ戻る
            candidatesPanel?.hide()
            updateClientState(client: c)
        }
    }

    /// パネル上の選択候補が変わったとき（矢印キー・クリックによるフォーカス移動）に呼ばれる。
    /// preedit を選択中の候補文字列で更新する。
    /// commit はここでは行わない（確定時は candidateSelected が呼ばれる）。
    override func candidateSelectionChanged(_ candidateString: NSAttributedString!) {
        guard let c = (currentSender as AnyObject?) ?? (self.client() as AnyObject?),
              let session else { return }

        let candidateText = candidateString.string
        logger.debug("candidateSelectionChanged: '\(candidateText)'")

        let segmentCount = Int(karukan_get_segment_count(session))
        if segmentCount > 0 {
            // 文節変換モード: 全文節を表示し、選択文節だけ候補テキストに置き換える
            let selectedSeg = Int(karukan_get_selected_segment(session))
            let segTexts = getSegmentTexts(
                session: session,
                segmentCount: segmentCount,
                overrideIndex: selectedSeg,
                overrideText: candidateText
            )
            let attrStr = buildSegmentAttrStr(segTexts: segTexts, selectedSeg: selectedSeg)
            let caretPos = segTexts.prefix(selectedSeg + 1).reduce(0) { $0 + $1.count }
            c.setMarkedText?(
                attrStr,
                selectionRange: NSRange(location: caretPos, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        } else {
            // 通常モード: 候補テキストをそのまま表示
            let attrStr = NSMutableAttributedString(string: candidateText)
            attrStr.addAttribute(
                .underlineStyle,
                value: NSUnderlineStyle.single.rawValue,
                range: NSRange(candidateText.startIndex..., in: candidateText)
            )
            c.setMarkedText?(
                attrStr,
                selectionRange: NSRange(location: candidateText.count, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }

        // カーソル位置を記録（末尾ラップ判定用）
        let count = Int(karukan_get_candidate_count(session))
        for i in 0..<count {
            if karukan_get_candidate(session, UInt32(i)).map({ String(cString: $0) }) == candidateText {
                candidateCursor = i
                break
            }
        }
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
        // バックグラウンドのライブ変換結果を無効化（deactivate 後のクライアント操作を防ぐ）
        liveConversionGeneration &+= 1
        liveConversionNeedsRetrigger = false
        currentSender = nil
        // karukan_session_init 未完了、またはセッション生成失敗の場合は session 関数に触らない。
        // 未初期化の KarukanSession にアクセスするとバックグラウンドスレッドと競合してクラッシュする。
        guard initialized, let session else {
            super.deactivateServer(sender)
            candidatesPanel?.hide()
            return
        }
        // sessionFinished 経由だと sender が無効な場合があるため、
        // insertText は行わず Rust 側の状態だけリセットする。
        // ただし hasPreedit が true の場合（marked text をアプリに送信済み）は
        // setMarkedText("") を呼んでアプリ側の stale marked text をクリアする。
        // こうしないと 英数 → かな の際にアプリが旧 marked text を確定して 0x10 等が入る。
        if karukan_is_empty(session) == 0 {
            _ = karukan_push_key(session, KarukanMacOSKey.returnKey.rawValue)
            _ = karukan_has_commit(session)  // commit テキストを消費して捨てる
        }
        if hasPreedit {
            (sender as AnyObject).setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            hasPreedit = false
        }
        karukan_save_learning(session)
        // super を先に呼んで IMKit 内部の deactivation を完了させる。
        // candidatesPanel?.hide() を super の前に呼ぶと、IMKit 内部のパネル参照と
        // 実際のパネル状態が食い違い、isVisible で無効なメモリにアクセスする場合がある。
        super.deactivateServer(sender)
        candidatesPanel?.hide()
    }

    override func commitComposition(_ sender: Any!) {
        logger.info("commitComposition")
        candidatesPanel?.hide()
        forceCommit(client: sender)
        // super.commitComposition は呼ばない:
        // macOS は 英数/かなキー切替時に commitComposition を自動呼び出しする。
        // Apple 基底クラスは handle() で消費した かな/英数キー（char = 0x10 DLE）を
        // 内部バッファに保持し、commitComposition 時に client へ flush する。
        // forceCommit で必要なテキストは自前で挿入済みのため super は不要かつ有害。
    }

    // -----------------------------------------------------------------------
    // MARK: - Private Helpers
    // -----------------------------------------------------------------------

    /// 初期化完了前にバッファしたキーイベントをリプレイする。
    /// メインスレッドから呼ぶこと。
    private func replayPendingEvents() {
        let events = pendingEvents
        pendingEvents.removeAll()
        logger.info("replaying \(events.count) buffered key events")
        for pending in events {
            _ = handle(pending.event, client: pending.sender)
        }
    }

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
            candidateCursor = 0
            panel.hide()
        }
    }

    private func updateClientState(client: Any?) {
        guard let session else { return }
        let c = client as AnyObject

        if karukan_has_commit(session) != 0 {
            let text = karukan_get_commit(session).map { String(cString: $0) } ?? ""
            if !text.isEmpty {
                logger.debug("insertText: '\(text, privacy: .public)'")
                c.insertText?(text, replacementRange: NSRange(location: NSNotFound, length: 0))
            }
        }

        let rawPreedit = karukan_get_preedit(session).map { String(cString: $0) } ?? ""
        // 制御文字（U+0000–U+001F, U+007F 等）を preedit から除去（Swift 側防御）。
        // Rust 側の apply_live_candidate フィルタの補完。setMarkedText に制御文字が渡らないことを保証。
        let preeditText = String(rawPreedit.unicodeScalars.filter { $0.value >= 0x20 })
        isComposing = !preeditText.isEmpty
        hasPreedit = !preeditText.isEmpty

        if preeditText.isEmpty {
            c.setMarkedText?(
                "",
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        } else {
            let segmentCount = Int(karukan_get_segment_count(session))
            if segmentCount > 0 {
                // 文節変換モード: 文節ごとに thick / single アンダーラインを設定
                let selectedSeg = Int(karukan_get_selected_segment(session))
                let segTexts = getSegmentTexts(session: session, segmentCount: segmentCount)
                let attrStr = buildSegmentAttrStr(segTexts: segTexts, selectedSeg: selectedSeg)
                let caretPos = segTexts.prefix(selectedSeg + 1).reduce(0) { $0 + $1.count }
                logger.debug("setMarkedText(bunsetsu): '\(preeditText, privacy: .public)' sel=\(selectedSeg)")
                c.setMarkedText?(
                    attrStr,
                    selectionRange: NSRange(location: caretPos, length: 0),
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
            } else {
                // 通常モード: ひらがな部分 = single、pending romaji = dotted underline
                let caretBytes = Int(karukan_get_preedit_caret(session))
                let cursorCharIndex = preeditText.utf8
                    .prefix(caretBytes)
                    .reduce(0) { acc, byte in
                        (byte & 0xC0) != 0x80 ? acc + 1 : acc
                    }
                let attrStr = NSMutableAttributedString(string: preeditText)
                let fullRange = NSRange(preeditText.startIndex..., in: preeditText)
                let romajiLen = Int(karukan_get_romaji_buf_len(session))
                if romajiLen > 0 {
                    // pending romaji は ASCII (1 byte = 1 UTF-16 code unit)
                    let totalChars = preeditText.utf16.count
                    let hiraganaChars = totalChars - romajiLen
                    if hiraganaChars > 0 {
                        attrStr.addAttribute(
                            .underlineStyle,
                            value: NSUnderlineStyle.single.rawValue,
                            range: NSRange(location: 0, length: hiraganaChars)
                        )
                    }
                    let dottedStyle = NSUnderlineStyle.single.rawValue | NSUnderlineStyle.patternDot.rawValue
                    attrStr.addAttribute(
                        .underlineStyle,
                        value: dottedStyle,
                        range: NSRange(location: hiraganaChars, length: romajiLen)
                    )
                } else {
                    attrStr.addAttribute(
                        .underlineStyle,
                        value: NSUnderlineStyle.single.rawValue,
                        range: fullRange
                    )
                }
                logger.debug("setMarkedText: '\(preeditText, privacy: .public)' caret=\(cursorCharIndex) romajiLen=\(romajiLen)")
                c.setMarkedText?(
                    attrStr,
                    selectionRange: NSRange(location: cursorCharIndex, length: 0),
                    replacementRange: NSRange(location: NSNotFound, length: 0)
                )
            }
        }

        // Empty 状態に戻ったら generation をインクリメントして残存タスクを無効化する
        if karukan_is_empty(session) != 0 {
            liveConversionGeneration &+= 1
        }
    }

    /// 全文節のテキストを `[String]` で返す。
    /// `overrideIndex` 番の文節を `overrideText` で上書きする（候補プレビュー用）。
    private func getSegmentTexts(
        session: OpaquePointer,
        segmentCount: Int,
        overrideIndex: Int = -1,
        overrideText: String = ""
    ) -> [String] {
        let fullPreedit = karukan_get_preedit(session).map { String(cString: $0) } ?? ""
        var texts: [String] = []
        var idx = fullPreedit.startIndex
        for i in 0..<segmentCount {
            let charCount = Int(karukan_get_segment_char_count(session, UInt32(i)))
            if i == overrideIndex {
                texts.append(overrideText)
                // preedit 上の文字数だけ進める（display が変わっていても）
                let end = fullPreedit.index(idx, offsetBy: charCount, limitedBy: fullPreedit.endIndex) ?? fullPreedit.endIndex
                idx = end
            } else {
                let end = fullPreedit.index(idx, offsetBy: charCount, limitedBy: fullPreedit.endIndex) ?? fullPreedit.endIndex
                texts.append(String(fullPreedit[idx..<end]))
                idx = end
            }
        }
        return texts
    }

    /// 文節テキスト配列から属性付き文字列を作成する。
    /// 選択文節は thick、それ以外は single アンダーライン。
    private func buildSegmentAttrStr(segTexts: [String], selectedSeg: Int) -> NSMutableAttributedString {
        let fullText = segTexts.joined()
        let attrStr = NSMutableAttributedString(string: fullText)
        var charOffset = 0
        for (i, seg) in segTexts.enumerated() {
            let len = seg.count
            if len > 0 {
                let style: Int = (i == selectedSeg)
                    ? NSUnderlineStyle.thick.rawValue
                    : NSUnderlineStyle.single.rawValue
                attrStr.addAttribute(
                    .underlineStyle,
                    value: style,
                    range: NSRange(location: charOffset, length: len)
                )
            }
            charOffset += len
        }
        return attrStr
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

        // ひらがな/カタカナが含まれない場合はライブ変換不要（記号のみ入力など）
        let hasKana = hiragana.unicodeScalars.contains {
            ($0.value >= 0x3041 && $0.value <= 0x3096) ||  // ひらがな
            ($0.value >= 0x30A0 && $0.value <= 0x30FF) ||  // カタカナ
            $0.value == 0x309D || $0.value == 0x309E ||    // ゝゞ
            $0.value == 0x30FC                              // ー
        }
        guard hasKana else { return }

        // 世代をインクリメント（前の推論が完了しても世代が違えば適用されない）
        liveConversionGeneration &+= 1
        let gen = liveConversionGeneration

        logger.debug("triggerLiveConversion: '\(hiragana)' gen=\(gen)")

        // self を strong capture してセッションが解放されないようにする。
        // deinit は DispatchQueue.main で動くため、このクロージャが完了するまで
        // karukan_session_free は呼ばれない。
        liveConversionNeedsRetrigger = false

        DispatchQueue.global(qos: .userInitiated).async { [self] in
            // 前の推論がまだ走っていれば今回はスキップ（同時推論を 1 件に制限）。
            // timeout: .now() = 非ブロッキング tryWait。取れなければ即 return。
            guard liveConversionSemaphore.wait(timeout: .now()) == .success else {
                logger.debug("live: skipped (inference busy) gen=\(gen)")
                // 推論完了後に最新状態で再トリガーするようフラグを立てる。
                // メインスレッドからのみ書き込まれるが、読み取りはバックグラウンドから
                // 行われないため排他不要。
                DispatchQueue.main.async { [weak self] in
                    self?.liveConversionNeedsRetrigger = true
                }
                return
            }
            defer {
                liveConversionSemaphore.signal()
                // 推論完了後: スキップされたトリガーがあれば最新状態で再実行する。
                DispatchQueue.main.async { [weak self] in
                    guard let self, self.liveConversionNeedsRetrigger else { return }
                    self.liveConversionNeedsRetrigger = false
                    self.triggerLiveConversion(sender: self.currentSender ?? self.client())
                }
            }

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
                // stale な結果は廃棄（deactivateServer でもインクリメントされる）
                guard self.liveConversionGeneration == gen else {
                    logger.debug("live conversion discarded (stale gen=\(gen))")
                    return
                }
                // deactivate 後はクライアントが無効なので操作しない
                let client = self.currentSender ?? self.client()
                guard client != nil else { return }

                logger.debug("apply_live_candidate: '\(autoCommitCandidate)' gen=\(gen)")
                if karukan_apply_live_candidate(session, autoCommitCandidate, hiragana) != 0 {
                    if hiragana.count > self.autoCommitMaxChars {
                        // 変換済みテキストをコミット（live_candidate が Some(漢字) の状態で Return）
                        _ = karukan_push_key(session, KarukanMacOSKey.returnKey.rawValue)
                        if !tailHiragana.isEmpty {
                            // 残余ひらがなを新規 Composing として注入し、ライブ変換を再トリガー
                            _ = karukan_set_composing_hiragana(session, tailHiragana)
                            self.updateClientState(client: client)
                            self.triggerLiveConversion(sender: client)
                            return
                        }
                    }
                    self.updateClientState(client: client)
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

    /// ASCII 記号 → 全角のマッピング。
    /// Rust（romaji converter）が消費しなかった記号に適用する。
    private static let fullWidthMap: [String: String] = [
        "!": "！", "?": "？",
        "/": "・", "\\": "＼",
        "(": "（", ")": "）",
        "[": "「", "]": "」",
        "{": "｛", "}": "｝",
        "<": "＜", ">": "＞",
        "~": "〜", "@": "＠",
        "#": "＃", "$": "＄",
        "%": "％", "^": "＾",
        "&": "＆", "*": "＊",
        "+": "＋", "=": "＝",
        "|": "｜", "_": "＿",
        ":": "：", ";": "；",
        "`": "｀",
        "'": "'", "\"": "\u{201D}",
    ]

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
        hasPreedit = false
    }

    // -----------------------------------------------------------------------
    // MARK: - 入力メニュー（メニューバードロップダウン）
    // -----------------------------------------------------------------------

    /// メニューを一度だけ構築する（init から呼ぶ）。
    /// IMKInputController.menu() のたびに再構築すると NSMenuItem.target (weak) が
    /// 切れるリスクがあるため、azooKey 方式でプロパティに固定する。
    private func setupMenu() {
        let menu = NSMenu(title: "Karukan")
        // autoenablesItems = false: レスポンダーチェーンによる自動 enable/disable を無効にする。
        // true（デフォルト）のままだとレスポンダーチェーンに self が含まれず
        // setConsonantDelay: 等が見つからない場合にアイテムが自動的にグレーアウトされる。
        menu.autoenablesItems = false

        // ライブ変換トグル
        let liveItem = NSMenuItem(
            title: "ライブ変換",
            action: #selector(toggleLiveConversion(_:)),
            keyEquivalent: ""
        )
        liveItem.target = self
        menu.addItem(liveItem)

        menu.addItem(.separator())

        // 変換ショートカット（preedit がないときはグレーアウト）
        let hiraganaItem = NSMenuItem(
            title: "ひらがなに変換",
            action: #selector(menuConvertHiragana(_:)),
            keyEquivalent: "j"
        )
        hiraganaItem.keyEquivalentModifierMask = .control
        hiraganaItem.target = self
        menu.addItem(hiraganaItem)

        let katakanaItem = NSMenuItem(
            title: "カタカナに変換",
            action: #selector(menuConvertKatakana(_:)),
            keyEquivalent: "k"
        )
        katakanaItem.keyEquivalentModifierMask = .control
        katakanaItem.target = self
        menu.addItem(katakanaItem)

        let asciiItem = NSMenuItem(
            title: "英字に変換",
            action: #selector(menuConvertAscii(_:)),
            keyEquivalent: ";"
        )
        asciiItem.keyEquivalentModifierMask = .control
        asciiItem.target = self
        menu.addItem(asciiItem)

        menu.addItem(.separator())

        // 子音遅延（フラット、インデント付き）
        // NOTE: IMKInputController.menu() ではサブメニューは OS に無視されるため
        //       ヘッダー行 + インデント付き選択肢のフラット構造で代替する。
        let delayHeader = NSMenuItem(title: "子音遅延", action: nil, keyEquivalent: "")
        delayHeader.isEnabled = false
        menu.addItem(delayHeader)
        for (label, value) in [
            ("なし", 0.0),
            ("0.05 秒", 0.05),
            ("0.10 秒", 0.10),
            ("0.15 秒", 0.15),
            ("0.20 秒", 0.20),
            ("0.30 秒", 0.30),
        ] {
            let item = NSMenuItem(
                title: "  \(label)",
                action: #selector(setConsonantDelay(_:)),
                keyEquivalent: ""
            )
            item.target = self
            item.tag = Int(value * 1000)
            item.indentationLevel = 1
            menu.addItem(item)
        }

        menu.addItem(.separator())

        // 自動コミット閾値（フラット、インデント付き）
        let commitHeader = NSMenuItem(title: "自動コミット閾値", action: nil, keyEquivalent: "")
        commitHeader.isEnabled = false
        menu.addItem(commitHeader)
        for chars in [10, 20, 30, 40, 50] {
            let item = NSMenuItem(
                title: "  \(chars) 文字",
                action: #selector(setAutoCommitMaxChars(_:)),
                keyEquivalent: ""
            )
            item.target = self
            item.tag = chars
            item.indentationLevel = 1
            menu.addItem(item)
        }

        menu.addItem(.separator())

        // 設定画面を開く
        let prefItem = NSMenuItem(
            title: "設定...",
            action: #selector(openPreferences(_:)),
            keyEquivalent: ""
        )
        prefItem.target = self
        menu.addItem(prefItem)

        appMenu = menu
    }

    /// メニュー表示直前にチェックマークを最新状態に更新する。
    private func updateMenuCheckmarks() {
        let currentDelay = consonantDelaySec
        let currentMax = autoCommitMaxChars

        for item in appMenu.items {
            switch item.action {
            case #selector(toggleLiveConversion(_:)):
                item.state = isLiveConversionEnabled ? .on : .off
            case #selector(setConsonantDelay(_:)):
                let v = Double(item.tag) / 1000.0
                item.state = abs(currentDelay - v) < 0.001 ? .on : .off
            case #selector(setAutoCommitMaxChars(_:)):
                item.state = currentMax == item.tag ? .on : .off
            default:
                break
            }
        }
    }

    override func menu() -> NSMenu! {
        updateMenuCheckmarks()
        return appMenu
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(menuConvertHiragana(_:)),
             #selector(menuConvertKatakana(_:)),
             #selector(menuConvertAscii(_:)):
            return isComposing
        default:
            return true
        }
    }

    @objc func menuConvertHiragana(_ sender: Any) {
        guard let session else { return }
        _ = karukan_push_key(session, KarukanMacOSKey.convertHiragana.rawValue)
        updateClientState(client: currentSender ?? client())
        candidatesPanel?.hide()
    }

    @objc func menuConvertKatakana(_ sender: Any) {
        guard let session else { return }
        _ = karukan_push_key(session, KarukanMacOSKey.convertKatakana.rawValue)
        updateClientState(client: currentSender ?? client())
        candidatesPanel?.hide()
    }

    @objc func menuConvertAscii(_ sender: Any) {
        guard let session else { return }
        _ = karukan_push_key(session, KarukanMacOSKey.convertAscii.rawValue)
        updateClientState(client: currentSender ?? client())
        candidatesPanel?.hide()
    }

    @objc func toggleLiveConversion(_ sender: Any) {
        isLiveConversionEnabled.toggle()
        logger.info("live conversion toggled via menu: \(self.isLiveConversionEnabled ? "enabled" : "disabled")")
        if !isLiveConversionEnabled, let session {
            _ = karukan_push_key(session, KarukanMacOSKey.escape.rawValue)
            updateClientState(client: currentSender ?? client())
        }
    }

    @objc func setConsonantDelay(_ sender: Any) {
        guard let item = Self.menuItem(from: sender) else {
            logger.error("setConsonantDelay: cannot extract NSMenuItem from \(type(of: sender))")
            return
        }
        let value = Double(item.tag) / 1000.0
        SettingStore.defaults.set(value, forKey: SettingStore.consonantDelaySecKey)
        logger.info("consonant delay changed via menu: \(value)s")
    }

    @objc func setAutoCommitMaxChars(_ sender: Any) {
        guard let item = Self.menuItem(from: sender) else {
            logger.error("setAutoCommitMaxChars: cannot extract NSMenuItem from \(type(of: sender))")
            return
        }
        let value = item.tag
        SettingStore.defaults.set(value, forKey: SettingStore.autoCommitMaxCharsKey)
        logger.info("auto commit max chars changed via menu: \(value)")
    }

    /// IMKit はメニューアクションの sender を NSMenuItem ではなく
    /// コマンド辞書（NSDictionary）として渡す。
    /// kIMKCommandMenuItemName キーから NSMenuItem を取り出すか、
    /// sender が直接 NSMenuItem の場合はそのまま返す。
    private static func menuItem(from sender: Any) -> NSMenuItem? {
        if let item = sender as? NSMenuItem { return item }
        if let dict = sender as? NSDictionary,
           let item = dict[kIMKCommandMenuItemName] as? NSMenuItem { return item }
        return nil
    }

    @objc func openPreferences(_ sender: Any) {
        // KarukanIM.app/Contents/Resources/KarukanPreferences.app を起動
        guard let resourceURL = Bundle.main.resourceURL else { return }
        let prefsURL = resourceURL.appendingPathComponent("KarukanPreferences.app")
        NSWorkspace.shared.openApplication(at: prefsURL, configuration: NSWorkspace.OpenConfiguration())
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
    case shrinkSegment   = 13  // KARUKAN_KEY_SHRINK_SEGMENT
    case extendSegment   = 14  // KARUKAN_KEY_EXTEND_SEGMENT

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
