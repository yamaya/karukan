# Phase 3.5 実装計画書: ライブ変換

> 前提: Phase 3（漢字変換 + 候補 UI）完了・動作確認済み
> 完了条件: タイピング中にリアルタイムで変換候補が preedit に反映される

---

## 概要

ライブ変換とは、スペースキーを押さなくても入力中のひらがなをリアルタイムで漢字変換し、preedit に反映し続ける機能。Enter で確定、Space で通常の候補選択モードへ移行する。

```text
n i h o n g o → にほんご（ライブ変換：日本語）
                                 ↓ Enter
                              「日本語」コミット

n i h o n g o → にほんご（ライブ変換：日本語）
                                 ↓ Space
                         候補ウィンドウ表示（Phase 3 と同じ）
```

---

## karukan-im との設計比較

Linux 版 `karukan-im` を参照した。ライブ変換の核心的な設計は同じだが、**macOS はメインスレッドをブロックできない**ため、非同期化が必要。

| 点 | karukan-im (Linux) | macOS |
|---|---|---|
| 状態 | `Composing` のまま、`live.text: String` フィールド | `Composing` のまま、`live_candidate: Option<String>` フィールド |
| 推論 | 同期（fcitx5 は独立プロセス、短時間ブロック可） | 非同期（メインスレッドブロック禁止） |
| Escape | 2段階（live クリア → 全キャンセル） | 同じ |
| Space | live_text を先頭候補として Conversion へ | 同じ |
| デバウンス | なし | なし（karukan-im に倣う） |

### karukan-im から学んだ重要な点

1. **新しい State は不要** — `SessionState::LiveConverting` は作らない。`Composing` のまま `live_candidate: Option<String>` フィールドだけで管理（`karukan-im` の `live.text` に相当）。

2. **2段階 Escape** — ライブ変換表示中の Escape は live_candidate をクリアしてひらがな表示に戻る。2回目でキャンセル。

3. **Space → Conversion 時の継続性** — `live_candidate` を `prev_suggest_text` として Conversion の先頭候補に保存（表示が途切れない）。

4. **Enter 確定** — `live_candidate` が Some なら変換済み文字をコミット、None ならひらがなをコミット（現状の `do_commit()` と同じ）。

---

## アーキテクチャ

### 状態遷移（Phase 3.5 追加分）

```text
【Phase 3 既存】
Empty ──push_char──► Composing
Composing ──Space──► Conversion
Conversion ──Return──► Empty

【Phase 3.5 追加】
Composing(live=None) ──[バックグラウンド推論完了]──► Composing(live=Some("日本語"))
Composing(live=Some) ──Return──► Empty          ★変換済みをコミット
Composing(live=Some) ──Escape──► Composing(live=None)  ★ひらがな表示に戻る
Composing(live=None) ──Escape──► Empty          ★全キャンセル（既存）
Composing(live=Some) ──Space──► Conversion      ★live_candidate を先頭候補に保存
```

### 非同期推論フロー

```text
[メインスレッド] push_char('o')
    │
    ├─ input_buf 更新
    ├─ preedit 更新（ひらがな表示）
    └─ triggerLiveConversion(hiragana="にほんご", gen=42)
           │
           ▼
   [DispatchQueue.global]
    karukan_convert_top1(session, "ニホンゴ")  ← Arc<KanaKanjiConverter> のみ使用（スレッドセーフ）
           │
           ▼ 結果: "日本語"
   [DispatchQueue.main.async]
    guard liveConversionGeneration == 42 else { return }  ← stale 廃棄
    karukan_apply_live_candidate(session, "日本語")
    flushPreedit()  ← preedit を "日本語" に更新
```

---

## Rust 側変更（`karukan-macos/src/session.rs`）

### 1. `KarukanSession` に `live_candidate` フィールドを追加

```rust
pub struct KarukanSession {
    state: SessionState,
    romaji: RomajiConverter,
    input_buf: InputBuffer,
    dict: Option<karukan_engine::Dictionary>,
    learning: Option<LearningCache>,
    converter: Option<Arc<KanaKanjiConverter>>,
    /// ライブ変換の結果。Some = 変換済み文字を preedit に表示中。
    /// karukan-im の `live.text` に相当。
    live_candidate: Option<String>,      // ← 追加
    pub(crate) preedit: PreeditCache,
    pub(crate) commit: CommitCache,
    pub(crate) candidate_cache: CandidateCache,
}
```

`new()` で `live_candidate: None` を追加。

### 2. `do_commit()` を修正（Enter 確定）

```rust
fn do_commit(&mut self) {
    // ローマ字バッファをフラッシュ
    let prev_len = self.romaji.output().chars().count();
    let _ = self.romaji.flush();
    let flushed: String = self.romaji.output().chars().skip(prev_len).collect();
    if !flushed.is_empty() {
        self.input_buf.insert(&flushed);
    }

    // ライブ変換中なら変換済みテキストをコミット（karukan-im と同じ）
    let committed = if let Some(live) = self.live_candidate.take() {
        let hiragana = self.input_buf.text.clone();
        if let Some(cache) = &mut self.learning {
            cache.record(&hiragana, &live);
        }
        live
    } else {
        std::mem::take(&mut self.input_buf.text)
    };

    self.input_buf.cursor_chars = 0;
    self.romaji.reset();
    self.state = SessionState::Empty;

    self.commit.text = CString::new(committed).unwrap_or_default();
    self.commit.dirty = true;
    self.update_preedit("");
}
```

### 3. `do_cancel()` を修正（2段階 Escape）

```rust
fn do_cancel(&mut self) {
    // karukan-im の cancel_composing() と同じ 2段階動作:
    // 1回目: live_candidate をクリアしてひらがな表示に戻る
    // 2回目: 全キャンセル
    if self.live_candidate.take().is_some() {
        let preedit_text = format!("{}{}", self.input_buf.text, self.romaji.buffer());
        self.update_preedit(&preedit_text);
        return;
    }

    self.romaji.reset();
    self.input_buf.clear();
    self.state = SessionState::Empty;
    self.update_preedit("");
}
```

### 4. `do_conversion()` を修正（Space → live_candidate を先頭候補に）

`collect_candidates()` の呼び出し後、`live_candidate` を先頭に挿入する（karukan-im の `prev_suggest_text` 処理）。

```rust
fn do_conversion(&mut self) {
    // ... 既存の romaji flush 処理 ...

    let hiragana = self.input_buf.text.clone();
    if hiragana.is_empty() { /* ... 全角スペース ... */ return; }

    // live_candidate を取り出し、候補の先頭に保存
    let prev_live = self.live_candidate.take();

    let mut candidates = self.collect_candidates(&hiragana);

    // live_candidate が候補リストにない場合のみ先頭に挿入
    // (表示していた変換が消えないようにする — karukan-im の prev_suggest_text と同じ)
    if let Some(live) = prev_live {
        if live != hiragana && !candidates.contains(&live) {
            candidates.insert(0, live);
        }
    }

    // ... 既存の CandidateCache 更新・SessionState::Conversion 遷移 ...
}
```

### 5. `apply_live_candidate()` を追加

```rust
/// バックグラウンドスレッドの推論結果を適用する（メインスレッドから呼ぶ）。
///
/// live_candidate をセットして preedit を変換済みテキストに更新する。
/// 入力が変わっていた場合は Swift 側の generation counter で弾くため、
/// ここでは無条件に適用する。
pub fn apply_live_candidate(&mut self, candidate: &str) {
    self.live_candidate = Some(candidate.to_string());

    // preedit: 変換済みテキストを表示（ひらがなの代わり）
    self.preedit.text = CString::new(candidate).unwrap_or_default();
    self.preedit.caret_bytes = candidate.len() as u32;
    self.preedit.dirty = true;
}
```

---

## Rust 側変更（`karukan-macos/src/ffi/`）

### 6. `ffi/query.rs` — `karukan_get_composing_hiragana` を追加

バックグラウンドスレッドが推論を起動する前に、現在のひらがなを取得するため。

```rust
/// Composing 状態の現在のひらがなを `buf` にコピーする。
/// 戻り値: コピーした文字数（null 終端含まず）。Composing でなければ 0。
/// スレッドセーフ: メインスレッドからのみ呼ぶこと。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_composing_hiragana(
    session: *const KarukanSession,
    buf: *mut c_char,
    buf_len: usize,
) -> c_int {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, 0);
        if !matches!(s.state, SessionState::Composing) {
            return 0;
        }
        let text = &s.input_buf.text;
        let bytes = text.as_bytes();
        let copy_len = bytes.len().min(buf_len.saturating_sub(1));
        if copy_len == 0 || buf.is_null() {
            return 0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, copy_len);
            *buf.add(copy_len) = 0;
        }
        copy_len as c_int
    })
    .unwrap_or(0)
}
```

### 7. `ffi/query.rs` — `karukan_convert_top1` / `karukan_free_string` を追加

バックグラウンドスレッドから安全に呼べる変換 API。**セッションのミュータブルな状態には触れず、`Arc<KanaKanjiConverter>` のみを使う**。

```rust
/// ひらがなをカタカナに変換して上位1候補を返す（ヒープ確保 CString）。
///
/// # スレッドセーフ
/// `session` の `converter` フィールドは `Arc<KanaKanjiConverter>` であり
/// `Send + Sync` のため、バックグラウンドスレッドから呼んでも安全。
/// ただし `session` が同時に解放・変更されないことを呼び出し側が保証すること。
///
/// 戻り値: UTF-8 文字列（`karukan_free_string` で解放）。失敗時は null。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_convert_top1(
    session: *const KarukanSession,
    hiragana_utf8: *const c_char,
) -> *mut c_char {
    std::panic::catch_unwind(|| {
        let s = ffi_ref!(session, std::ptr::null_mut());
        let Some(conv) = s.converter.as_ref() else {
            return std::ptr::null_mut();
        };
        let hiragana = unsafe { std::ffi::CStr::from_ptr(hiragana_utf8) }
            .to_str()
            .unwrap_or("");
        let katakana = karukan_engine::kana::hiragana_to_katakana(hiragana);
        match conv.convert(&katakana, "", 1) {
            Ok(candidates) if !candidates.is_empty() => {
                CString::new(candidates[0].clone())
                    .map(|s| s.into_raw())
                    .unwrap_or(std::ptr::null_mut())
            }
            _ => std::ptr::null_mut(),
        }
    })
    .unwrap_or(std::ptr::null_mut())
}

/// `karukan_convert_top1` が返したポインタを解放する。
#[unsafe(no_mangle)]
pub extern "C" fn karukan_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe { drop(CString::from_raw(ptr)); }
    }
}
```

### 8. `ffi/input.rs` — `karukan_apply_live_candidate` を追加

```rust
/// バックグラウンド推論の結果を適用する。メインスレッドからのみ呼ぶこと。
///
/// 戻り値: 1=適用成功（preedit が dirty）、0=Composing 状態でない（無視）
#[unsafe(no_mangle)]
pub extern "C" fn karukan_apply_live_candidate(
    session: *mut KarukanSession,
    candidate_utf8: *const c_char,
) -> c_int {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let s = ffi_mut!(session);
        if !matches!(s.state, SessionState::Composing) {
            return 0;
        }
        let candidate = unsafe { std::ffi::CStr::from_ptr(candidate_utf8) }
            .to_str()
            .unwrap_or("");
        s.apply_live_candidate(candidate);
        1
    }))
    .unwrap_or(0)
}
```

### 9. `karukan-macos/include/karukan_macos.h` に宣言を追加

```c
// ── ライブ変換 ──────────────────────────────────────────────────────────────

/// Composing 状態のひらがなを buf にコピーする。
/// バックグラウンドスレッドで呼ぶ前にメインスレッドで取得すること。
int karukan_get_composing_hiragana(
    const KarukanSession* session,
    char* buf,
    size_t buf_len);

/// ひらがなをカタカナ変換して上位1候補を返す（ヒープ確保）。
/// Arc<KanaKanjiConverter> のみ使用するためバックグラウンドスレッドから呼べる。
/// 戻り値は karukan_free_string で解放すること。
char* karukan_convert_top1(
    const KarukanSession* session,
    const char* hiragana_utf8);

/// karukan_convert_top1 の戻り値を解放する。
void karukan_free_string(char* ptr);

/// バックグラウンド推論結果を session に適用する。メインスレッドからのみ呼ぶこと。
/// 戻り値: 1=適用成功, 0=Composing 状態でないため無視
int karukan_apply_live_candidate(
    KarukanSession* session,
    const char* candidate_utf8);
```

---

## Swift 側変更（`KarukanInputController.swift`）

### 10. generation counter とライブ変換トリガー

```swift
/// ライブ変換の世代カウンタ。
/// push_char のたびにインクリメントし、stale な推論結果を廃棄するために使う。
/// karukan-im にはデバウンスがないため、ここでも設けない。
private var liveConversionGeneration: Int = 0

/// バックグラウンドで推論を起動し、結果をメインスレッドで適用する。
///
/// - session からひらがなを読み取り（メインスレッド）
/// - Arc<KanaKanjiConverter> のみ使って変換（バックグラウンド）
/// - generation が一致すれば apply（メインスレッド）
private func triggerLiveConversion(sender: Any?) {
    guard let session else { return }

    // メインスレッドでひらがなを取得
    var buf = [CChar](repeating: 0, count: 256)
    let len = karukan_get_composing_hiragana(session, &buf, buf.count)
    guard len > 0 else { return }
    let hiragana = String(cString: buf)

    // 世代をインクリメント
    liveConversionGeneration &+= 1
    let gen = liveConversionGeneration

    DispatchQueue.global(qos: .userInitiated).async { [weak self] in
        guard let self else { return }

        // Arc<KanaKanjiConverter> のみ触るのでスレッドセーフ
        let resultPtr = karukan_convert_top1(session, hiragana)
        guard let resultPtr else { return }
        defer { karukan_free_string(resultPtr) }
        let candidate = String(cString: resultPtr)

        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            // stale な結果を廃棄
            guard self.liveConversionGeneration == gen else { return }

            if karukan_apply_live_candidate(session, candidate) != 0 {
                self.flushPreedit(sender: sender ?? self.client())
            }
        }
    }
}
```

### 11. `handle(_:client:)` での呼び出し

`push_char` の後にライブ変換を起動する。

```swift
override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
    guard initialized, let session else { return false }
    currentSender = sender
    // ...（既存の modifier チェック）...

    var consumed = false

    if let key = KarukanMacOSKey.from(keyCode: event.keyCode) {
        consumed = karukan_push_key(session, key.rawValue) != 0
    } else if let chars = event.characters, /* printable */ {
        consumed = chars.withCString { karukan_push_char(session, $0) != 0 }

        // 文字入力後にライブ変換をトリガー
        if consumed {
            triggerLiveConversion(sender: sender)
        }
    }

    updateClientState(client: sender)
    updateCandidatesPanel(sender: sender)
    return consumed
}
```

### 12. generation counter のリセット

preedit がクリアされる（Empty 状態に戻る）タイミングで generation を無効化する。`updateClientState` 内、またはコミット後に行う。

```swift
// Empty になったらカウンタをリセット（残存タスクが apply しても弾かれるよう世代を変える）
if karukan_is_empty(session) != 0 {
    liveConversionGeneration &+= 1
}
```

---

## preedit 表示の変化

| 状態 | preedit の内容 |
|---|---|
| Composing, live=None | ひらがな（下線付き）例: `にほんご` |
| Composing, live=Some | 変換済みテキスト 例: `日本語` |
| Conversion | 選択中候補（ハイライト）例: `日本語` |

---

## テスト要件

### ライブ変換の基本動作

```text
[ ] "nihongo" 入力中に候補ウィンドウなしで preedit が「日本語」になる
[ ] Enter で「日本語」がコミットされる（ひらがなでなく変換済み）
[ ] Escape 1回目: preedit が「にほんご」に戻る
[ ] Escape 2回目: 入力全キャンセル（preedit 消える）
```

### Space → Conversion との連携

```text
[ ] ライブ変換中（「日本語」表示）に Space → 候補ウィンドウが開く
[ ] その際「日本語」が先頭候補として残っている
```

### 学習キャッシュとの連携

```text
[ ] Enter でライブ変換をコミットすると learning.tsv に記録される
[ ] 次回同じ読みで learning 候補が先頭に来る
```

### 非同期安全性

```text
[ ] 高速タイピング中にクラッシュしない
[ ] stale な推論結果が混入しない（世代カウンタで弾かれる）
[ ] モデルロード前（converter = None）でもクラッシュしない
```

---

## 既知リスク

| # | リスク | 対処 |
|---|---|---|
| R1 | バックグラウンド中に session が解放される | `weak self` + `guard session` で防ぐ |
| R2 | 推論が重く毎キー起動するとスレッド飽和 | `DispatchQueue.global` は最大 64 スレッドまでキューイングするため通常問題ない。重い場合はシリアルキューに変更 |
| R3 | `karukan_convert_top1` が session の他フィールドに触れるバグ | コードレビューで `converter` フィールドのみアクセスすることを確認 |
| R4 | ライブ変換結果がひらがなと同じ（モデル未ロード時など）| `apply_live_candidate` を適用するが視覚変化なし。許容範囲 |

---

## 完了条件（Acceptance Criteria）

- [ ] `cargo build -p karukan-macos` がエラーなく成功する
- [ ] "nihongo" 入力中にスペース不要で「日本語」が preedit に表示される
- [ ] Enter で「日本語」がコミットされる
- [ ] Escape 2段階が正しく動く（live クリア → 全キャンセル）
- [ ] Space で候補ウィンドウが開き、live 候補が先頭に残っている
- [ ] 高速タイピング（100ms/key 以下）でクラッシュしない
- [ ] `log stream --predicate 'subsystem == "com.example.karukan"'` でエラーログが出ない

---

## Phase 4 への引き継ぎ事項

1. ライブ変換の **有効/無効トグル**（Ctrl+Shift+L 相当）— Phase 4 で設定 UI を追加する際に実装。
2. ライブ変換で文節分割が必要なケース（長い文）— 現在は全体を1候補として扱う。将来的には karukan-im の `ParallelBeam` 戦略を参考に複数候補を活用する。
3. **パフォーマンス計測** — `karukan_convert_top1` の実行時間を計測し、100ms を超える場合はシリアルキューまたはキャンセル機構の導入を検討する。
