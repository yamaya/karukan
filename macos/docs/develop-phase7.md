# macOS ユーザー辞書サポート — 設計・実装計画

## 背景

Linux 版 (`karukan-im`) では既にユーザー辞書が完全実装されている。macOS 版 (`karukan-macos`) では `paths::user_dict_dir()` が定義済みだが、`session.rs` の `init_resources()` でロードしていない。候補優先度の組み立て (`build_candidates`) にもユーザー辞書スロットがない。

本計画は Linux 版の実装をリファレンスに、macOS 版にユーザー辞書サポートを追加する。

## 仕様

### TSV フォーマット (Mozc/Google IME 互換)

```tsv
# karukan ユーザー辞書
# ヨミ<TAB>表層形<TAB>品詞<TAB>コメント
# 品詞・コメントは省略可能
かるかん	Karukan
あずーきー	azooKey
ごーるでんうぃーく	GW	短縮よみ
```

- `#` で始まる行はコメント（スキップ）
- 最低2カラム（ヨミ, 表層形）。3・4カラム目（品詞, コメント）は任意
- ヨミはひらがな（`Dictionary::load_auto` 内部で Unicode 正規化される）
- エンコーディング: UTF-8

### 辞書ファイル配置

```text
~/Library/Application Support/Karukan/user_dicts/
├── my_dict.tsv
├── names.tsv
└── tech_terms.tsv        # 複数ファイル可。ファイル名ソート順で優先度決定
```

- `paths::user_dict_dir()` = `~/Library/Application Support/Karukan/user_dicts/`
- ディレクトリ内の全ファイルを `Dictionary::load_auto()` で読み込み
    - KRKN バイナリ形式と Mozc TSV 形式を自動判別
- ファイル名アルファベット順でソート → 先のファイルが高優先度
- 複数辞書は `Dictionary::merge()` で統合

### 候補優先度

```text
1. 学習キャッシュ (Learning)
2. ユーザー辞書  (UserDictionary)  ← 新規追加
3. ニューラルモデル (Model)
4. システム辞書 (Dictionary)
5. フォールバック (ひらがな/カタカナ)
```

Linux 版と同一の優先度順。

### 利用手順（UI なし）

1. `~/Library/Application Support/Karukan/user_dicts/` にTSVファイルを配置
2. KarukanIM.app を再起動（ログアウト→ログイン、または `killall KarukanIM`）
3. 辞書のエントリが変換候補に反映される

## 実装計画

### Step 1: `KarukanSession` にユーザー辞書フィールドを追加

**ファイル**: `karukan-macos/src/session.rs`

`KarukanSession` 構造体に `user_dict` フィールドを追加:

```rust
pub struct KarukanSession {
    // ... 既存フィールド ...
    dict: Option<karukan_engine::Dictionary>,
    user_dict: Option<karukan_engine::Dictionary>,  // ← 追加
    // ...
}
```

`new()` で `user_dict: None` を初期化。

### Step 2: `init_resources()` でユーザー辞書をロード

**ファイル**: `karukan-macos/src/session.rs`

`init_resources()` に以下のロジックを追加（システム辞書ロードの直後）:

```rust
// User dictionaries (optional — scan user_dicts/ directory)
let user_dict_dir = paths::user_dict_dir();
if user_dict_dir.exists() {
    if let Ok(entries) = std::fs::read_dir(&user_dict_dir) {
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();

        let mut dicts = Vec::new();
        for path in &paths {
            match karukan_engine::Dictionary::load_auto(path) {
                Ok(dict) => {
                    tracing::info!("Loaded user dictionary from {:?}", path);
                    dicts.push(dict);
                }
                Err(e) => tracing::warn!("Failed to load user dictionary {:?}: {}", path, e),
            }
        }

        if !dicts.is_empty() {
            match karukan_engine::Dictionary::merge(dicts) {
                Ok(Some(merged)) => {
                    tracing::info!("User dictionaries merged ({} files)", paths.len());
                    self.user_dict = Some(merged);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Failed to merge user dictionaries: {}", e),
            }
        }
    }
}
```

Linux 版 `init.rs:init_user_dictionaries()` とほぼ同じロジック。

### Step 3: `build_candidates()` にユーザー辞書を組み込む

**ファイル**: `karukan-macos/src/session.rs`

現在の `build_candidates()` (L928付近):

```text
1. Learning cache
2. Neural model (beam search)
3. System dictionary
```

これを以下に変更:

```text
1. Learning cache
2. User dictionary      ← 追加
3. Neural model (beam search)
4. System dictionary
5. フォールバック (ひらがな)
```

具体的な変更:

```rust
fn build_candidates(&self, hiragana: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();

    // 1. Learning cache (highest priority)
    if let Some(cache) = &self.learning {
        for (surface, _score) in cache.lookup(hiragana) {
            if !result.contains(&surface) {
                result.push(surface);
            }
        }
    }

    // 2. User dictionary (higher than model/system dict)
    if let Some(dict) = &self.user_dict {
        if let Some(lr) = dict.exact_match_search(hiragana) {
            for c in &lr.candidates {
                if !result.contains(&c.surface) {
                    result.push(c.surface.clone());
                }
            }
        }
    }

    // 3. Neural model candidates (beam search)
    if let Some(conv) = &self.converter {
        match conv.convert(hiragana, "", 9) {
            Ok(model_cands) => {
                for c in model_cands {
                    if !result.contains(&c) {
                        result.push(c);
                    }
                }
            }
            Err(e) => tracing::warn!("KanaKanjiConverter::convert failed: {}", e),
        }
    }

    // 4. System dictionary
    if let Some(dict) = &self.dict {
        if let Some(lr) = dict.exact_match_search(hiragana) {
            for c in lr.candidates.iter().take(5) {
                if !result.contains(&c.surface) {
                    result.push(c.surface.clone());
                }
            }
        }
    }

    // 5. Fallback
    if result.is_empty() {
        result.push(hiragana.to_string());
    }
    result
}
```

### Step 4: テスト

**ファイル**: `karukan-macos/src/session.rs` (既存テストモジュールに追加)

```rust
#[test]
fn test_user_dict_loaded_and_prioritized() {
    // KARUKAN_DATA_DIR を一時ディレクトリに設定
    // user_dicts/ に TSV ファイルを作成
    // init_resources() を呼び出し
    // build_candidates() でユーザー辞書エントリが
    //   モデル・システム辞書より前に出ることを確認
}
```

- `KARUKAN_DATA_DIR` 環境変数でテスト用ディレクトリを使用
- モデルなし（`converter: None`）でも辞書候補が正しく返ることを確認
- 空ディレクトリ、不正ファイル、複数ファイルのケースをカバー

## 変更対象ファイル

| ファイル | 変更内容 |
|---|---|
| `karukan-macos/src/session.rs` | `user_dict` フィールド追加、`init_resources()` にロード処理、`build_candidates()` に辞書検索追加 |

## 追加不要なもの

- **karukan-engine 側の変更**: 不要。`Dictionary::load_auto()`, `Dictionary::merge()`, Mozc TSV パーサーは全て既存
- **paths.rs**: `user_dict_dir()` は既に定義済み
- **FFI 層**: 変更不要。候補リストの構築は Rust 側で完結
- **Swift 側**: 変更不要。候補の表示ロジックは既存のまま
- **UI**: 今回スコープ外

## リスク・注意点

- Mozc TSV は起動時にダブルアレイトライを毎回構築するため、大規模辞書（1万語超）ではロード時間が増加する。将来的には `karukan-dict build` で事前バイナリ化を推奨
- App Sandbox 環境下では `~/Library/Application Support/Karukan/` は実際にはコンテナ内 (`~/Library/Containers/com.example.inputmethod.KarukanIM/Data/Library/Application Support/Karukan/`) にリダイレクトされる。ユーザーへの案内時に注意が必要
