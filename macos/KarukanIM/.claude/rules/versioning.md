# Versioning

## ビルド設定変数

| 変数 | Info.plist キー | 管理方法 |
|------|----------------|----------|
| `MARKETING_VERSION` | `CFBundleShortVersionString` | 手動 (`agvtool`) |
| `CURRENT_PROJECT_VERSION` | `CFBundleVersion` | Release ビルド時に自動 (git コミット数) |

`CFBundleGetInfoString` は `$(MARKETING_VERSION) ($(CURRENT_PROJECT_VERSION))` で構成される。

## バージョン更新手順

### マーケティングバージョン（例: 1.0.0 → 1.1.0）

```bash
cd macos/KarukanIM
agvtool new-marketing-version 1.1.0
```

`project.pbxproj` の全 configuration で `MARKETING_VERSION` が更新される。

### ビルド番号（CFBundleVersion）

手動操作は不要。Release ビルド時に Run Script ("Set Build Number") が `git rev-list --count HEAD` で自動設定する。

手動で合わせたい場合のみ:

```bash
agvtool new-version -all $(git -C ../../ rev-list --count HEAD)
```

### CFBundleGetInfoString

手動操作は不要。Release ビルド時に Run Script が `MARKETING_VERSION` と `BUILD_NUMBER` から自動生成する。

## 注意事項

- Info.plist にバージョン番号を直書きしない。必ず `$(MARKETING_VERSION)` / `$(CURRENT_PROJECT_VERSION)` を使う。
- Run Script は KarukanIM・KarukanIMExtension 両ターゲットに設定済み。
