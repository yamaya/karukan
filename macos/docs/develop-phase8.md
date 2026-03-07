# 文節ナビゲーション — 設計・実装計画

## 背景

スペースキーで変換モードに入った後、左右矢印キーで文節を移動し、各文節ごとに候補を選べる機能を追加する。macOS 標準 IME や Google IME と同等の文節変換 UX を実現する。

## 仕様

### キー操作

| キー | 状態 | 動作 |
|---|---|---|
| Space | Composing | 文節変換モードへ（最初の文節を選択 + 候補パネル表示） |
| ← Left | BunsetsuConversion | 前の文節を選択（候補パネルを閉じる） |
| → Right | BunsetsuConversion | 次の文節を選択（候補パネルを閉じる） |
| Space / Tab / ↓ | BunsetsuConversion（パネル非表示時） | 選択文節の候補パネルを表示 |
| Space / Tab / ↓ | BunsetsuConversion（パネル表示中） | 次候補へ（IMKCandidates が処理） |
| ↑ | BunsetsuConversion（パネル表示中） | 前候補へ（IMKCandidates が処理） |
| Return | BunsetsuConversion（パネル非表示） | 全文節を確定コミット |
| Return | BunsetsuConversion（パネル表示中） | 候補を確定 → 文節ナビに戻る |
| Escape | BunsetsuConversion（パネル表示中） | 候補パネルを閉じる（文節ナビは継続） |
| Escape | BunsetsuConversion（パネル非表示） | 変換キャンセル → Composing（ひらがな復元） |
| Backspace | BunsetsuConversion | 変換キャンセル → Composing → 末尾1文字削除 |

### 表示

- **選択文節**: thick アンダーライン（`NSUnderlineStyle.thick`）
- **非選択文節**: single アンダーライン（`NSUnderlineStyle.single`）
- 候補パネルの候補移動中も全文節を表示し、選択文節の表示が候補テキストに追従する

### ライブ変換との連携

| 状態 | キー | 動作 |
|---|---|---|
| Composing（ライブ変換あり） | Left | BunsetsuConversion に入り、**最後の文節**を選択 |
| Composing（ライブ変換なし） | Left | アプリにパススルー（ひらがなカーソル移動、従来通り） |
| BunsetsuConversion | — | `apply_live_candidate` が Composing 状態チェックで自動的に無視 |

**ライブ変換中 Left の意図**: ユーザーは変換済みテキスト全体を見ながら「末尾を直したい」と判断して Left を押す。したがって最後の文節が選択された状態で入る。

---

## 状態機械の変更

### 変更前

```
Empty → Composing → Conversion(ConversionState)
                    ├── candidates: Vec<String>
                    ├── hiragana: String
                    └── cursor: usize
```

### 変更後

```
Empty → Composing → BunsetsuConversion(BunsetsuConversionState)
                    ├── segments: Vec<BunsetsuSegment>
                    │   ├── hiragana: String
                    │   ├── display: String
                    │   └── candidates: Vec<String>
                    └── selected: usize
```

候補パネルの表示/非表示は `candidate_cache.items` の空/非空で制御（既存方式を踏襲）。

---

## 文節分割アルゴリズム

形態素解析器（MeCab 等）は不使用。助詞ベースのヒューリスティックで分割する。

**規則**：
- 2文字助詞（優先）: `から` `まで` `より` `って` `けど` `ので` `のに` `には` `では` `とは` `でも` `とも`
- 1文字助詞: `は` `が` `を` `に` `で` `へ` `と` `も` `の` `や` `か`
- 助詞をその文節の末尾に含め、直後で分割する
- 助詞の後ろに文字がない場合は分割しない
- 分割結果が 0 の場合は全体を 1 文節とする

**例**:

```
"わたしはがっこうへいきます"
→ ["わたしは", "がっこうへ", "いきます"]

"せんたく"
→ ["せんたく"]  （単一文節）

"きょうはいいてんきですね"
→ ["きょうは", "いいてんきですね"]
```

---

## 実装計画

### Step 1: `session.rs` — 状態定義を置き換える

**ファイル**: `karukan-macos/src/session.rs`

`ConversionState` / `SessionState::Conversion` を削除し、新しい型に置き換える:

```rust
struct BunsetsuSegment {
    hiragana: String,
    display: String,
    candidates: Vec<String>,
}

struct BunsetsuConversionState {
    segments: Vec<BunsetsuSegment>,
    selected: usize,
}

enum SessionState {
    Empty,
    Composing,
    BunsetsuConversion(BunsetsuConversionState),
}
```

### Step 2: `session.rs` — `segment_hiragana()` 自由関数を追加

```rust
/// ひらがな文字列を助詞境界で文節に分割する。
/// 助詞はその前の文節に含める。分割できなければ全体を 1 要素で返す。
fn segment_hiragana(hiragana: &str) -> Vec<String> { ... }
```

実装のポイント:
- `chars()` でイテレート。最低 1 文字はセグメントに含める
- 2文字助詞を 1文字より優先チェック
- 助詞の直後に文字がある場合のみ分割（末尾の助詞は分割しない）

### Step 3: `session.rs` — `do_conversion()` を置き換える

`do_conversion_impl(start_at_last: bool)` を実装し、`do_conversion()` はそのラッパーとする:

```rust
fn do_conversion(&mut self) { self.do_conversion_impl(false); }
```

`do_conversion_impl` の処理:

1. ローマ字バッファをフラッシュ
2. ひらがなが空なら全角スペースをコミット（既存動作を維持）
3. `prev_live = self.live_candidate.take()`
4. `segment_hiragana(hiragana)` で文節に分割
5. 各文節に対して `collect_candidates(hiragana)` で候補を収集
6. 単一文節かつ `prev_live` がある場合: 候補先頭に挿入
7. `initial_selected = if start_at_last { segments.len() - 1 } else { 0 }`
8. `initial_selected` 番の文節の候補を `candidate_cache` に設定（パネル即表示）
9. `SessionState::BunsetsuConversion` へ遷移（`selected: initial_selected`）
10. `preedit` = 全文節の `display` を結合したテキスト（キャレットは選択文節末尾）

### Step 4: `session.rs` — `push_key()` を更新

`SessionState::Conversion` のハンドラを `BunsetsuConversion` に置き換える:

```
Return     → commit_bunsetsu_all()
Escape     → candidate_cache が空なら cancel_bunsetsu()、非空なら candidate_cache.clear()
Backspace  → cancel_bunsetsu() + do_backspace()
Left       → move_segment(-1)
Right      → move_segment(1)
Space/Tab/Down → show_segment_candidates()
Up         → consume（true を返す）
```

**Composing 状態での Left 追加処理**:

```rust
KarukanKey::Left
    if matches!(self.state, SessionState::Composing)
        && self.live_candidate.is_some() =>
{
    // ライブ変換中 Left: 最後の文節を選択状態で BunsetsuConversion に入る
    self.do_conversion_impl(/*start_at_last=*/ true);
    true
}
```

ライブ変換がない Composing 状態での Left は引き続き `false` を返し、アプリに委ねる（ひらがなカーソル移動）。

`push_char()` での `cancel_conversion()` 呼び出しも `cancel_bunsetsu()` に置き換える。

### Step 5: `session.rs` — 新メソッドを追加

#### `move_segment(delta: i32)`

```rust
fn move_segment(&mut self, delta: i32) {
    // selected を ±1（clamp）
    // candidate_cache を空にする（パネルを隠す）
    // preedit を全文節の display で更新（caret = 選択文節末尾）
}
```

#### `show_segment_candidates()`

```rust
fn show_segment_candidates(&mut self) {
    // segments[selected].candidates を candidate_cache に設定
}
```

#### `commit_bunsetsu_all()`

```rust
fn commit_bunsetsu_all(&mut self) {
    // 全 segments の display を結合してコミット
    // display != hiragana の文節を学習キャッシュに記録
    // SessionState::Empty へ遷移
}
```

#### `cancel_bunsetsu()`

```rust
fn cancel_bunsetsu(&mut self) {
    // segments の hiragana を結合してひらがな文字列を復元
    // SessionState::Composing へ遷移し、preedit をひらがなで更新
}
```

### Step 6: `session.rs` — `select_candidate()` を更新

```rust
pub fn select_candidate(&mut self, index: usize) -> bool {
    // BunsetsuConversion 状態の場合:
    //   segments[selected].display を更新
    //   学習キャッシュに記録
    //   candidate_cache を空にする（パネルを隠す）
    //   commit.dirty = false のまま（コミットしない）
    //   preedit を更新して返す
    // （他の状態は false を返す）
}
```

Swift の `candidateSelected` が `karukan_has_commit() == 0` を確認してパネルを隠し `updateClientState` を呼ぶことで、段階的な文節選択 UI を実現する。

### Step 7: `session.rs` — クエリメソッドを追加

```rust
pub fn segment_count(&self) -> usize { ... }
pub fn selected_segment(&self) -> usize { ... }
pub fn segment_char_count(&self, index: usize) -> usize { ... }
```

`segment_char_count` は **`display` の `chars().count()`** を返す（UTF-16 BMP 範囲の日本語文字は 1 コードポイント = 1 NSString 文字）。

### Step 8: `session.rs` — `restore_hiragana_if_conversion()` を更新

`Conversion` → `BunsetsuConversion` に対応させる:

```rust
fn restore_hiragana_if_conversion(&mut self) {
    let hiragana = match &self.state {
        SessionState::BunsetsuConversion(conv) => {
            Some(conv.segments.iter().map(|s| s.hiragana.as_str()).collect::<String>())
        }
        _ => None,
    };
    if let Some(hiragana) = hiragana { ... }
}
```

### Step 9: `ffi/query.rs` — セグメント情報 FFI を追加

```rust
#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_segment_count(session: *const KarukanSession) -> u32 { ... }

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_segment_char_count(
    session: *const KarukanSession,
    index: u32,
) -> u32 { ... }

#[unsafe(no_mangle)]
pub extern "C" fn karukan_get_selected_segment(session: *const KarukanSession) -> u32 { ... }
```

### Step 10: `karukan_macos.h` — ヘッダーに宣言を追加

```c
/** BunsetsuConversion 状態のセグメント数を返す。0 なら非文節変換状態。 */
uint32_t karukan_get_segment_count(const KarukanSession* session);

/** セグメント index の現在表示テキストの文字数（NSString 長）を返す。 */
uint32_t karukan_get_segment_char_count(const KarukanSession* session, uint32_t index);

/** 現在選択中のセグメントインデックスを返す。 */
uint32_t karukan_get_selected_segment(const KarukanSession* session);
```

### Step 11: `KarukanInputController.swift` — 候補パネル表示中の Left/Right 処理

候補パネル表示中のキーハンドリングブロックに追加:

```swift
case 123: // Left → 候補パネルを閉じて前の文節へ
    guard let session else { return true }
    _ = karukan_push_key(session, KarukanMacOSKey.left.rawValue)
    updateClientState(client: sender)
    panel.hide()
case 124: // Right → 候補パネルを閉じて次の文節へ
    guard let session else { return true }
    _ = karukan_push_key(session, KarukanMacOSKey.right.rawValue)
    updateClientState(client: sender)
    panel.hide()
```

### Step 12: `KarukanInputController.swift` — `candidateSelected` を更新

```swift
override func candidateSelected(_ candidateString: NSAttributedString!) {
    // ... インデックス逆引き・select_candidate 呼び出し（既存）...

    if karukan_has_commit(session) != 0 {
        // 通常コミット（BunsetsuConversion 以外、または将来の全文節一括確定）
        // ... 既存コード ...
        c.setMarkedText?("", ...)
    } else {
        // 文節候補の確定: コミットなし、preedit を更新して文節ナビへ戻る
        candidatesPanel?.hide()
        updateClientState(client: c)
    }
}
```

### Step 13: `KarukanInputController.swift` — `candidateSelectionChanged` を更新

候補パネルで矢印キーを動かしたとき、全文節を表示しつつ選択文節に候補テキストを反映する:

```swift
override func candidateSelectionChanged(_ candidateString: NSAttributedString!) {
    guard let session else { return }
    let segmentCount = Int(karukan_get_segment_count(session))

    if segmentCount > 0 {
        // 全文節テキストを構築（選択文節だけ候補テキストで上書き）
        let selectedSeg = Int(karukan_get_selected_segment(session))
        let segTexts = getSegmentTexts(session: session, segmentCount: segmentCount,
                                        overrideIndex: selectedSeg,
                                        overrideText: candidateString.string)
        let attrStr = buildSegmentAttrStr(segTexts: segTexts, selectedSeg: selectedSeg)
        let caretPos = segTexts[...selectedSeg].reduce(0) { $0 + $1.count }
        c.setMarkedText?(attrStr, selectionRange: NSRange(location: caretPos, length: 0), ...)
    } else {
        // 既存の動作（単一文字列をアンダーライン付きで表示）
    }
}
```

### Step 14: `KarukanInputController.swift` — `updateClientState` を更新

`karukan_get_segment_count > 0` のとき、文節ごとに異なるアンダーラインを描画する:

```swift
let segmentCount = Int(karukan_get_segment_count(session))
if !preeditText.isEmpty && segmentCount > 0 {
    // 文節変換モード: thick / single アンダーラインを文節ごとに設定
    let selectedSeg = Int(karukan_get_selected_segment(session))
    let segTexts = getSegmentTexts(session: session, segmentCount: segmentCount)
    let attrStr = buildSegmentAttrStr(segTexts: segTexts, selectedSeg: selectedSeg)
    let caretPos = segTexts[...selectedSeg].reduce(0) { $0 + $1.count }
    c.setMarkedText?(attrStr, selectionRange: NSRange(location: caretPos, length: 0), ...)
} else if !preeditText.isEmpty {
    // 既存の動作（単一アンダーライン）
}
```

#### ヘルパー関数

**`getSegmentTexts(session:segmentCount:overrideIndex:overrideText:)`**

`karukan_get_preedit` の文字列を `karukan_get_segment_char_count` で分割し `[String]` を返す。`overrideIndex` が指定された場合はそのインデックスを `overrideText` で上書きする。

**`buildSegmentAttrStr(segTexts:[String], selectedSeg:Int)`**

`segTexts` を結合し、各文節に `.underlineStyle` を設定した `NSMutableAttributedString` を返す:
- `selectedSeg` の文節: `NSUnderlineStyle.thick.rawValue`
- それ以外: `NSUnderlineStyle.single.rawValue`

---

## 変更対象ファイル

| ファイル | 変更内容 |
|---|---|
| `karukan-macos/src/session.rs` | `ConversionState` 削除、`BunsetsuConversionState` 追加、関連メソッド全面更新 |
| `karukan-macos/src/ffi/query.rs` | `karukan_get_segment_count/char_count/selected_segment` 追加 |
| `karukan-macos/include/karukan_macos.h` | 3 関数の C 宣言を追加 |
| `macos/KarukanIM/KarukanIM/KarukanInputController.swift` | パネル表示中 Left/Right ハンドリング、`candidateSelected` / `candidateSelectionChanged` / `updateClientState` 更新 |

---

## 追加不要なもの

- **karukan-engine の変更**: 不要。`collect_candidates` は既存の `collect_candidates()` メソッドを流用
- **形態素解析器**: 不要。助詞ベースのヒューリスティックで十分
- **新しい FFI 関数（`select_candidate` 以外）**: 変更なし。`karukan_select_candidate` が BunsetsuConversion でも動作するよう内部実装を変更するだけ

---

## リスク・注意点

1. **助詞ベース分割の精度**: 形態素解析を使わないため、「でも」を助詞として分割してしまうケースがある（例:「でもいい」→「でも」＋「いい」ではなく「でもいい」のまま保ちたい）。初期実装として許容し、将来的に改善可能
2. **`move_candidate` の除去**: 現行コードの `move_candidate()` は実際には呼ばれない（パネル表示中は Swift が Space を IMKCandidates に委譲するため）。除去しても動作は変わらない
3. **`NSRange` の文字カウント**: `karukan_get_segment_char_count` は `display.chars().count()` を返す。日本語は全て BMP 範囲（1 コードポイント = 1 NSString 文字 = 1 Swift `Character`）なので NSRange.length と一致する
4. **Backspace 動作**: パネル表示中の Backspace は Swift が Escape として Rust に送る（既存動作）。Escape は `candidate_cache` をクリアして文節ナビに戻る。パネル非表示時の Backspace は `cancel_bunsetsu() + do_backspace()` でひらがなに戻り末尾を削除する

---

## 検証方法

```bash
# ビルド
cargo build -p karukan-macos --release
cd macos/KarukanIM && xcodebuild -scheme KarukanIMExtension build

# テスト（Rust 単体）
cargo test -p karukan-macos

# 手動動作確認シナリオ
# 1. "せんたく" + Space → 候補パネル表示、"選択" thick アンダーライン
# 2. "わたしはがっこうへ" + Space → 2文節以上に分割、最初の文節選択
# 2b. ライブ変換中に Left → 最後の文節を選択して BunsetsuConversion に入る
# 3. Left → 前の文節に移動（パネルが隠れる）
# 4. Right → 次の文節に移動
# 5. Space → 選択文節の候補パネル表示
# 6. ↓ → 次候補へ（preedit の選択文節が更新）
# 7. Return → 文節候補を確定、文節ナビに戻る
# 8. Return（パネル非表示）→ 全文節を確定コミット
# 9. Escape（パネル表示中）→ パネルを閉じて文節ナビへ
# 10. Escape（パネル非表示）→ Composing（ひらがな）に戻る
```
