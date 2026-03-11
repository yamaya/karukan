# 候補メニュー：件数拡張とスクロール末尾ラップ — 設計・実装計画

## 背景

候補メニューに表示される件数が常に10件前後に留まっており、変換候補を十分に選べない問題がある。原因はRust側の `collect_candidates()` がモデル最大9件・辞書最大5件しか返さないこと、および最後の候補でSpaceを押しても `moveDown(nil)` が止まるだけで先頭に戻れないことの2点。

本フェーズでは候補収集上限を引き上げ（最大~25件）、スクロール末尾での先頭ラップを実装する。

## 設計方針

`candidates()` は全件を渡し続ける（`kIMKSingleColumnScrollingCandidatePanel` を維持）。Swift側でカーソル位置を追跡し、最終項目でSpaceが押されたら `panel.update()` で先頭に巻き戻す。

### キー操作（候補パネル表示中）

| キー | 動作 |
|---|---|
| Space | 通常: 次候補へ（`panel.moveDown`）。最終項目: 先頭へラップ（`panel.update`） |
| Shift-Space | 前候補へ（`panel.moveUp`）。先頭で止まる |
| Tab | Space と同じ |
| Shift-Tab | Shift-Space と同じ |
| Down | 次候補へ（変更なし） |
| Up | 前候補へ（変更なし） |
| Return | 確定（変更なし） |
| Escape / Backspace | キャンセル（変更なし） |

### ラップの仕組み

```
Space（最終項目）
  → panel.update()
  → IMKCandidates が candidates() を再取得し、カーソルを先頭にリセット
  → candidateSelectionChanged(先頭候補) が発火
  → preedit と candidateCursor が先頭候補に更新される
```

### デメリット・既知の課題

- `panel.update()` でカーソルが先頭にリセットされるかは IMKCandidates の内部実装に依存（Apple非公開）
- `candidateCursor` は `candidateSelectionChanged` 経由でのみ更新されるため、IMKit内部のカーソル移動と同期がずれる可能性がある
- Shift-Space での末尾ラップは IMKCandidates に任意位置へジャンプするAPIがないため実装不可（先頭で止まる）

## 変更箇所

### `karukan-macos/src/session.rs`

候補収集の上限引き上げ（~25件に拡張）:

```rust
// L1462: モデル beam search
conv.convert(hiragana, "", 9)  →  conv.convert(hiragana, "", 15)

// L1477: システム辞書
.take(5)  →  .take(10)
```

### `macos/KarukanIM/KarukanIM/KarukanInputController.swift`

**プロパティ追加**:
```swift
/// 候補パネルの現在のカーソル位置（0-based）。
/// candidateSelectionChanged で更新し、末尾ラップ判定に使う。
private var candidateCursor: Int = 0
```

**`candidateSelectionChanged(_:)`**: 既存の末尾に `candidateCursor` 更新処理を追加。

**`updateCandidatesPanel(sender:)`**: `panel.hide()` 時に `candidateCursor = 0` をリセット。

**パネル表示中のSpace(49)/Tab(48)処理**: 末尾ラップロジックを追加。

**`candidates(_:)`・`candidateSelected(_:)`**: 変更なし。

## データフロー

```
Space × 2
  → Rust: collect_candidates → 最大25件をキャッシュ
  → panel.show() / panel.update()
  → candidates() が全25件を返す → スクロールリスト表示

Space（通常）
  → panel.moveDown(nil) → candidateSelectionChanged → candidateCursor++ / preedit更新

Space（最終項目）
  → candidateCursor == total-1 → panel.update()
  → candidates() 再取得 → カーソル先頭リセット
  → candidateSelectionChanged(#0) → candidateCursor=0 / preedit更新

Return
  → candidateSelected → Rust全候補から文字列逆引き → karukan_select_candidate(絶対idx)
```

## 確認方法

1. `cargo build -p karukan-macos --release`
2. Xcode で KarukanIM をリビルド・インストール
3. 動作確認:
   - Space × 2 → 10件以上の候補が表示されるか
   - Space 連打で1件ずつ下にスクロールするか
   - 最終候補でSpace → 先頭候補に戻るか（ラップ）
   - Shift-Space で逆方向移動し、先頭で止まるか
   - Return / クリックで正しくコミットされるか
   - Left/Right で文節変更後に候補を開き直したとき先頭から始まるか
