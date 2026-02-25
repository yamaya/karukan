# Phase 3 実装計画書: 漢字変換 + 候補 UI 統合

> ブランチ: `feature/macos-phase3-candidates`
> 前提: `feature/macos-phase2-imk` 完了・マージ済み
> 完了条件: スペースキーで変換候補を表示し、選択して漢字をコミットできること

---

## 目標

`karukan-engine` の `KanaKanjiConverter`（llama.cpp ベース）を統合し、
ひらがな → 漢字変換を macOS 上で動作させる。

```text
konnnichiha [Space] → 「こんにちは / 今日は / 今日和 ...」候補ウィンドウ表示
                      ↓ 選択 [Return]
                      「今日は」コミット
```

---

## 現状確認（Phase 2 完了時点）

| 項目 | 状態 |
|---|---|
| ローマ字 → ひらがな | ✅ 動作中 |
| `KanaKanjiConverter` 統合 | ❌ 未実装（`session.rs` の `init_resources` に TODO あり） |
| Space キーの動作 | ⚠️ 全角スペース挿入（Phase 3 で変換トリガーに変更） |
| `karukan_get_candidate_count` / `karukan_get_candidate` | ❌ 未実装（ヘッダーに宣言済みなら有効化） |
| `IMKCandidates` | ❌ 未実装 |
| 辞書・学習キャッシュの変換への反映 | ❌ 未実装（ロードのみ） |

---

## アーキテクチャ

### 変換フロー

```text
[Space 押下]
    │
    ▼
karukan_push_key(SPACE)
    │
    ├─ Composing 状態 かつ input_buf が空でない
    │       │
    │       ▼
    │   KanaKanjiConverter::convert(hiragana, context, n=9)
    │       │
    │       ├─ Dict / Learning で候補を補完・リランク
    │       │
    │       ▼
    │   SessionState::Conversion に遷移
    │   candidates = [...] 確定
    │       │
    │       ▼
    │   karukan_get_candidate_count() > 0
    │
    │
[Swift 側]
    │
    ▼
IMKCandidates.update(sender)
IMKCandidates.show(kIMKLocateCandidatesAboveHint)
    │
    ▼
候補選択 → candidateSelected(_:)
    │
    ▼
karukan_select_candidate(session, index)
    │
    ▼
karukan_has_commit() → insertText()
```

### 状態遷移（Phase 3 追加分）

```text
Empty ──push_char──► Composing
Composing ──Return──► Empty        (ひらがなコミット: Phase 2 動作)
Composing ──Escape──► Empty        (キャンセル: Phase 2 動作)
Composing ──Space──► Conversion    ★Phase 3 追加
Conversion ──Return──► Empty       (選択候補コミット)
Conversion ──Escape──► Composing   (変換キャンセル、ひらがなに戻る)
Conversion ──Space / Tab / Down──► Conversion (次候補)
Conversion ──Up / Shift-Tab──► Conversion    (前候補)
Conversion ──数字キー──► Empty     (番号指定コミット: 任意実装)
```

---

## Rust 側変更（`karukan-macos`）

### 1. `session.rs`

#### `SessionState` に `Conversion` を追加

```rust
enum SessionState {
    Empty,
    Composing,
    Conversion(ConversionState),
}

struct ConversionState {
    /// 変換対象のひらがな（戻る際に使う）
    hiragana: String,
    /// 変換候補リスト（Learning → Dict → Model 順でリランク済み）
    candidates: Vec<String>,
    /// 現在選択中の候補インデックス
    cursor: usize,
}
```

#### `KarukanSession` に `converter` フィールドを追加

```rust
pub struct KarukanSession {
    // ... 既存フィールド ...
    converter: Option<KanaKanjiConverter>,
    /// 候補キャッシュ（FFI から参照される CString リスト）
    pub(crate) candidate_cache: CandidateCache,
}

pub(crate) struct CandidateCache {
    pub items: Vec<CString>,  // 各候補の CString
    pub cursor: u32,
}
```

#### `init_resources` にモデルロードを追加

```rust
pub fn init_resources(&mut self) {
    // ... 既存の dict / learning ロード ...

    // モデルロード（HuggingFace から自動 DL、初回のみ）
    match Backend::from_variant_id("default") {
        Ok(backend) => match KanaKanjiConverter::new(backend) {
            Ok(conv) => {
                tracing::info!("KanaKanjiConverter loaded");
                self.converter = Some(conv);
            }
            Err(e) => tracing::warn!("Failed to init converter: {}", e),
        },
        Err(e) => tracing::warn!("Failed to load backend: {}", e),
    }
}
```

> **注意**: `init_resources` は Swift 側でバックグラウンドスレッドから呼ばれるため、
> モデルのダウンロード（初回のみ）を含む重い処理でも問題ない。

#### `push_key(Space)` を変換トリガーに変更

```rust
(SessionState::Composing, KarukanKey::Space) => {
    // Phase 1 の全角スペース挿入を削除し、変換を起動
    self.do_conversion();
    true
}

fn do_conversion(&mut self) {
    // ローマ字バッファをフラッシュしてひらがなを確定
    let prev_len = self.romaji.output().chars().count();
    let _ = self.romaji.flush();
    let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
    if !flushed.is_empty() {
        self.input_buf.insert(&flushed);
    }

    let hiragana = self.input_buf.text.clone();
    if hiragana.is_empty() {
        // 空なら全角スペースをコミット（Phase 1 互換）
        self.commit.text = CString::new("\u{3000}").unwrap_or_default();
        self.commit.dirty = true;
        self.state = SessionState::Empty;
        self.update_preedit("");
        return;
    }

    // 変換候補を取得
    let mut candidates = self.collect_candidates(&hiragana);
    if candidates.is_empty() {
        candidates.push(hiragana.clone());
    }

    // 候補キャッシュを更新
    self.candidate_cache.items = candidates
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap_or_default())
        .collect();
    self.candidate_cache.cursor = 0;

    self.state = SessionState::Conversion(ConversionState {
        hiragana,
        candidates,
        cursor: 0,
    });

    // preedit に先頭候補を表示（下線付き）
    self.update_preedit_for_conversion();
}

/// 候補収集: Learning → Dict → Model の優先順でマージ
fn collect_candidates(&self, hiragana: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    // 1. 学習キャッシュ（最優先）
    if let Some(cache) = &self.learning {
        if let Some(learned) = cache.get_top(hiragana) {
            result.push(learned.to_string());
        }
    }

    // 2. モデル変換（最大 9 候補）
    if let Some(conv) = &self.converter {
        match conv.convert(hiragana, "", 9) {
            Ok(model_cands) => {
                for c in model_cands {
                    if !result.contains(&c) {
                        result.push(c);
                    }
                }
            }
            Err(e) => tracing::warn!("Conversion failed: {}", e),
        }
    }

    // 3. システム辞書（フォールバック）
    if let Some(dict) = &self.dict {
        for entry in dict.lookup(hiragana).into_iter().take(5) {
            if !result.contains(&entry.surface) {
                result.push(entry.surface.clone());
            }
        }
    }

    // 候補がなければ読みをそのまま返す
    if result.is_empty() {
        result.push(hiragana.to_string());
    }
    result
}
```

#### Conversion 状態のキーハンドラ

```rust
(SessionState::Conversion(ref mut conv), KarukanKey::Return) => {
    let selected = conv.candidates[conv.cursor].clone();
    // 学習キャッシュに記録
    if let Some(cache) = &mut self.learning {
        cache.record(&conv.hiragana, &selected);
    }
    self.commit.text = CString::new(selected).unwrap_or_default();
    self.commit.dirty = true;
    self.state = SessionState::Empty;
    self.update_preedit("");
    true
}
(SessionState::Conversion(ref mut conv), KarukanKey::Escape) => {
    // ひらがな編集状態に戻す
    let hiragana = conv.hiragana.clone();
    self.state = SessionState::Composing;
    self.update_preedit(&hiragana);
    true
}
(SessionState::Conversion(ref mut conv), KarukanKey::Space)
| (SessionState::Conversion(ref mut conv), KarukanKey::Tab)
| (SessionState::Conversion(ref mut conv), KarukanKey::Down) => {
    conv.cursor = (conv.cursor + 1) % conv.candidates.len();
    self.candidate_cache.cursor = conv.cursor as u32;
    self.update_preedit_for_conversion();
    true
}
(SessionState::Conversion(ref mut conv), KarukanKey::Up) => {
    conv.cursor = if conv.cursor == 0 {
        conv.candidates.len() - 1
    } else {
        conv.cursor - 1
    };
    self.candidate_cache.cursor = conv.cursor as u32;
    self.update_preedit_for_conversion();
    true
}
```

#### `karukan_select_candidate` のセッション側実装

```rust
/// 候補インデックスを指定して即確定する（IMKCandidates のクリック用）
pub fn select_candidate(&mut self, index: usize) -> bool {
    let SessionState::Conversion(ref conv) = self.state else {
        return false;
    };
    if index >= conv.candidates.len() {
        return false;
    }
    let selected = conv.candidates[index].clone();
    let hiragana = conv.hiragana.clone();
    if let Some(cache) = &mut self.learning {
        cache.record(&hiragana, &selected);
    }
    self.clear_flags();
    self.commit.text = CString::new(selected).unwrap_or_default();
    self.commit.dirty = true;
    self.state = SessionState::Empty;
    self.update_preedit("");
    true
}
```

---

### 2. `ffi/query.rs` に候補 API を追加

```rust
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate_count(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| {
        ffi_ref!(session, 0).candidate_cache.items.len() as u32
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate(
    session: *const KarukanSession,
    index: u32,
) -> *const c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null());
        s.candidate_cache
            .items
            .get(index as usize)
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
    .unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_candidate_cursor(session: *const KarukanSession) -> u32 {
    std::panic::catch_unwind(|| ffi_ref!(session, 0).candidate_cache.cursor).unwrap_or(0)
}
```

### 3. `ffi/input.rs` に `karukan_select_candidate` を追加

```rust
#[unsafe(no_mangle)]
pub extern "C" fn karukan_select_candidate(
    session: *mut KarukanSession,
    index: u32,
) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        if ffi_mut!(session).select_candidate(index as usize) { 1 } else { 0 }
    }))
    .unwrap_or(0)
}
```

### 4. `karukan-macos/include/karukan_macos.h` に宣言を追加

```c
// ── 候補 ─────────────────────────────────────────────
/// 変換候補の数を返す。変換中でなければ 0。
uint32_t    karukan_get_candidate_count(const KarukanSession* session);

/// index 番目の候補テキスト（null 終端 UTF-8）を返す。
/// ポインタは次の push_* / select_candidate 呼び出しまで有効。
const char* karukan_get_candidate(const KarukanSession* session, uint32_t index);

/// 現在選択中の候補インデックスを返す。
uint32_t    karukan_get_candidate_cursor(const KarukanSession* session);

/// 候補を index で選択してコミットする。
/// 戻り値: 1=成功, 0=失敗（変換中でないか範囲外）
int         karukan_select_candidate(KarukanSession* session, uint32_t index);
```

---

## Swift 側変更（`KarukanInputController.swift`）

### 変更点サマリー

1. `IMKCandidates` プロパティを追加
2. `init` で `IMKCandidates` を生成
3. `handle(_:client:)` で候補ウィンドウの表示/非表示を制御
4. `candidates(_:)` を実装（Rust から候補を取得）
5. `candidateSelected(_:)` を実装（クリック選択）

```swift
import InputMethodKit
import OSLog

private let logger = Logger(subsystem: "com.example.karukan", category: "InputController")

@objc(KarukanInputController)
final class KarukanInputController: IMKInputController {

    private var session: OpaquePointer?
    private var initialized = false
    /// 候補ウィンドウ（IMKServer と 1:1）
    private var candidatesPanel: IMKCandidates?

    // MARK: - Lifecycle

    override init!(server: IMKServer!, delegate: Any!, client: Any!) {
        super.init(server: server, delegate: delegate, client: client)
        guard let ptr = karukan_session_new() else { return }
        session = ptr

        // 候補パネルを生成（セッションごとではなくサーバーごとに 1 つ）
        candidatesPanel = IMKCandidates(
            server: server,
            panelType: kIMKSingleColumnScrollingCandidatePanel
        )

        let captured = ptr
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let ret = karukan_session_init(captured)
            logger.info("karukan_session_init: \(ret)")
            DispatchQueue.main.async { self?.initialized = true }
        }
    }

    // MARK: - Key Handling

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard initialized, let session else { return false }
        guard event.type == .keyDown else { return false }

        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if flags.contains(.command) || flags.contains(.option) || flags.contains(.control) {
            return false
        }

        if let key = KarukanMacOSKey.from(keyCode: event.keyCode) {
            let consumed = karukan_push_key(session, key.rawValue) != 0
            updateClientState(client: sender)
            updateCandidatesPanel(sender: sender)
            return consumed
        }

        guard let chars = event.characters,
              let scalar = chars.unicodeScalars.first,
              scalar.value >= 0x20 else { return false }

        let consumed = chars.withCString { karukan_push_char(session, $0) != 0 }
        updateClientState(client: sender)
        updateCandidatesPanel(sender: sender)
        return consumed
    }

    // MARK: - IMKCandidates データソース

    /// Rust から候補を取得して IMKCandidates に渡す。
    override func candidates(_ sender: Any!) -> [Any]! {
        guard let session else { return [] }
        let count = Int(karukan_get_candidate_count(session))
        return (0..<count).compactMap { idx in
            karukan_get_candidate(session, UInt32(idx)).map { String(cString: $0) }
        }
    }

    /// 候補ウィンドウでクリック or Return 選択された。
    override func candidateSelected(_ candidateString: NSAttributedString!) {
        guard let session else { return }

        // 文字列からインデックスを逆引きして select_candidate を呼ぶ
        let text = candidateString.string
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

        // クライアントにコミット
        if karukan_has_commit(session) != 0,
           let ptr = karukan_get_commit(session) {
            let committed = String(cString: ptr)
            if !committed.isEmpty {
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

    // MARK: - Private

    /// 候補数に応じてパネルを表示/非表示する。
    private func updateCandidatesPanel(sender: Any?) {
        guard let panel = candidatesPanel, let session else { return }
        let count = karukan_get_candidate_count(session)
        if count > 0 {
            panel.update(sender)
            panel.show(kIMKLocateCandidatesAboveHint)
        } else {
            panel.hide()
        }
    }
}
```

---

## モデルダウンロード

`karukan_session_init` → `init_resources` → `Backend::from_variant_id("default")` が
HuggingFace から GGUF を自動ダウンロードする（`karukan-engine` の既存実装）。

保存先: `~/Library/Application Support/Karukan/models/`

**初回起動時の注意**:
- ダウンロードはバックグラウンドスレッドで実行されるためメインスレッドはブロックしない
- ダウンロード完了前に Space キーを押した場合、候補は辞書・学習キャッシュのみになる
- `initialized` フラグが `true` になってからモデルロード完了まで数秒かかる可能性がある
- ログ: `karukan_session_init returned: 0` が出力されれば完了

---

## テスト要件

### 基本変換

```text
[x] "nihongo" [Space] → 候補ウィンドウに「日本語」等が表示される
[x] 候補を選択 [Return] → 「日本語」がコミットされる
[x] "nihongo" [Space] [Space] → 次の候補に移動する
[x] "nihongo" [Space] [Escape] → ひらがな編集状態に戻る（「にほんご」preedit 表示）
[~] 候補ウィンドウでクリック → candidateSelected が呼ばれてコミットされる
    - コミットはされなかった
```

### 学習キャッシュ

```text
[~] 「日本語」を選択コミット後、再度 "nihongo" [Space] → 「日本語」が先頭候補になる
    - キャッシュが効いてない
```

### エッジケース

```text
[ ] モデルロード前に [Space] → 辞書のみの候補が表示される（クラッシュしない）
[x] 変換中にフォーカスを失う → deactivateServer で先頭候補をコミット
[ ] 変換候補が 1 件の場合 → パネル表示（1 件でもパネルを出す）
```

---

## 既知リスク

| # | リスク | 対処 |
|---|---|---|
| R1 | llama.cpp の `generate_beam_search` が重く候補表示に時間がかかる | `num_candidates` を減らす / greedy decoding でまず 1 候補を出す |
| R2 | `App Sandbox + HuggingFace ダウンロード` が失敗する | `com.apple.security.network.client` entitlement が必要（Phase 4 で追加） |
| R3 | `IMKCandidates` のパネルが preedit と被る / 位置がずれる | `kIMKLocateCandidatesAboveHint` を他のヒントに変えて調整 |
| R4 | `Dictionary::lookup` のシグネチャが不明 | karukan-engine の API を確認後実装 |
| R5 | モデルなし環境で `Backend::from_variant_id` がパニック | `Result` 型で受けているため `catch_unwind` で保護済み |

---

## 完了条件（Acceptance Criteria）

- [x] `cargo build -p karukan-macos` がエラーなく成功する
- [x] ビルド後 `~/Library/Input Methods/KarukanIM.app` に自動インストールされる
- [x] "nihongo" → Space で候補ウィンドウが開く
- [x] Return で選択候補がコミットされる
- [x] Escape でひらがなに戻る
- [ ] 2 回目の変換で学習した候補が先頭に来る
- [ ] クラッシュなし（Console.app でクラッシュログが出ない）

---

## Phase 4 への引き継ぎ事項

1. **`ENABLE_HARDENED_RUNTIME = NO`** は Phase 4 で `YES` に戻す必要がある。
   llama.cpp が JIT または unsigned executable memory を使う場合は
   `com.apple.security.cs.allow-unsigned-executable-memory` entitlement が必要（要動作確認）。

2. **`ENABLE_APP_SANDBOX = YES` + ネットワーク**: HuggingFace DL のために
   `com.apple.security.network.client` を entitlements に追加する。

3. **`com.example.inputmethod.KarukanIM`** は仮 Bundle ID。
   Phase 4 で Apple Developer Program の正式 ID に変更する。
