# GitHub 公開前チェックリスト

**作成日**: 2026-03-08
**対象ブランチ**: `epic/macos`

## ブロッカー（push 前に必須） ... 済み

### xcuserdata がトラッキングされている

```text
macos/KarukanIM/KarukanIM.xcodeproj/xcuserdata/goron.xcuserdatad/xcschemes/xcschememanagement.plist
```

- ユーザー名 `goron` がパスに含まれる個人ファイル
- `.gitignore` に `xcuserdata/` はあるが、すでに `git add` 済みでトラッキング中
- 対処:

```bash
git rm --cached 'macos/KarukanIM/KarukanIM.xcodeproj/xcuserdata/goron.xcuserdatad/xcschemes/xcschememanagement.plist'
git commit -m "build(xcode): remove user-specific scheme configuration"
```


## 要検討（push 前に判断）

### Bundle ID が `com.example.*` のまま ... 済み

- `io.github.yamaya.inputmethod.KarukanIM` に変更済み
- `io.github.yamaya.inputmethod.karukan.Japanese` に変更済み
- 変更箇所: `Info.plist` ×2、`project.pbxproj`、Swift ソース（Logger/suiteName/connection）

### `macos/docs/security-audit.md` が未追跡 ... 済み

- `git status` に `??` で表示されている
- コミットする / `.gitignore` に追加する / 削除する、どれかに決める

### `macos/docs/develop-phase*.md` が公開される ... 済み

- 開発経緯メモとして公開する方針で確定
- コードブロックを除去し、状態遷移図・フロー図を Mermaid に書き直し済み

### リモートが `git@github.com:yamaya/karukan.git` ... 済み

- `yamaya/karukan` が意図通りの公開先であることを確認済み

---

## 確認済み（問題なし）

| 項目 | 状態 |
|------|------|
| ハードコードされたシークレット・APIキー | なし（`HF_TOKEN` は環境変数のみ） |
| ローカルパスの埋め込み（`/Users/goron/` 等） | なし |
| サンドボックス設定 | 適切（`network.client` は HF ダウンロード用として正当） |
| ライセンスファイル | あり（MIT + Apache-2.0） |
| ビルド成果物の除外 | `target/`, `DerivedData/`, `Build/` は `.gitignore` 済み |
| 不審な外部通信 | なし（HuggingFace モデルDLのみ） |
| `unsafe` ブロック | 限定的（FFI とテストのみ） |
