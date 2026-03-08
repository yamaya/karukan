# セキュリティ監査レポート

**日付**: 2026-03-08
**対象**: karukan リポジトリ全体（Rust + Swift）
**結論**: バックドア・悪意あるコードは検出されず

---

## 調査範囲

- Rust コード: karukan-engine, karukan-macos, karukan-cli, karukan-im
- Swift コード: macos/KarukanIM
- 依存関係: Cargo.toml / Cargo.lock
- 設定ファイル: models.toml, entitlements

---

## 調査結果

| 項目 | 結果 | 備考 |
|------|------|------|
| ハードコードされた認証情報 | ✅ なし | `HF_TOKEN` は環境変数経由のみ |
| 不審なネットワーク通信 | ✅ なし | HuggingFace モデルDLのみ |
| 動的コード実行 (`exec`/`eval`) | ✅ なし | llama.cpp の `eval_sequence` は推論処理（正当） |
| 隠れたファイル操作 | ✅ なし | 学習キャッシュ・辞書のみ |
| 難読化・エンコード文字列 | ✅ なし | `include_str!` は models.toml 埋め込みのみ |
| 不審な依存クレート | ✅ なし | すべて公式ライブラリ |
| Swift 側ネットワーク呼び出し | ✅ なし | URLSession 未使用 |
| `unsafe` ブロック | ✅ 限定的 | テストと C FFI のみ |

---

## 主要な確認事項

### ネットワーク通信
唯一の外部通信は `karukan-engine/src/kanji/hf_download.rs` での HuggingFace モデルダウンロード。
接続先は `models.toml` で明示定義（`togatogah/jinen-v1-*.gguf`）。

### ファイル書き込み先
- `~/Library/Application Support/Karukan/learning.tsv` — 学習キャッシュ
- `~/Library/Application Support/Karukan/user_dicts/` — ユーザー辞書
- `~/Library/Application Support/Karukan/dict.bin` — システム辞書

### macOS サンドボックス
`com.apple.security.network.client = true` は HuggingFace ダウンロード用として正当。

---

## 評価

**セキュリティリスク: 低 — 安全に使用できる**
