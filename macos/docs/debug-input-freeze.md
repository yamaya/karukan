# 入力不能バグ（input freeze）の調査手順

## 症状

- 入力ソースとしては認識されており、macOS標準の"かな"に切り替えることは可能
- KarukanIM が選択された状態でキーを押しても何も起きない

---

## 原因仮説

### 仮説A: `karukan_session_init` ハング（最有力）

`KarukanInputController.init` はリソースロード（辞書・モデル）をバックグラウンドで実行し、
完了後にメインスレッドで `initialized = true` をセットする。

```
DispatchQueue.global.async {
    karukan_session_init(...)          // ← ここでハング
    DispatchQueue.main.async {
        self.initialized = true        // ← 到達しない
    }
}
```

`initialized` が `false` のままだと `handle(_:client:)` が
`guard initialized else { return true }` で全キーを黙って消費し、何も出力されない。

**確認ポイント:** ログに `initialized = true` が出ているか。

### 仮説B: 候補パネルのゾンビ状態

候補パネル（`IMKCandidates`）が表示中にフォーカスを失い、`hide()` が呼ばれずに
`isVisible()` が `true` のまま残ることがある。

この状態で `handle(_:client:)` に入ると全キーがパネルルートに吸い込まれ
（`return true` で消費）、ユーザーには何も届かない。

**対策:** `activateServer` でパネルを強制 `hide()` する実装を追加済み（2026-03-14）。
ただし `warning` ログで記録されるため、発生していればログで確認できる。

### 仮説C: Rust セッションパニック

`push_char` / `push_key` 内でパニックが起きると状態機械が壊れる。
クラッシュログに記録される。

---

## 調査手順

### 1. リアルタイムログ（再現前から流しておく）

```bash
log stream \
  --predicate 'subsystem == "io.github.yamaya.karukan"' \
  --level debug \
  --style compact 2>&1 | tee /tmp/karukan-debug.log
```

### 2. 症状が出たらプロセス確認

```bash
# プロセスが生きているか
ps aux | grep -E '[Kk]arukan'

# スレッドスタック（ハング箇所を特定）
sudo sample $(pgrep -x KarukanIMExtension) 5 -file /tmp/karukan-sample.txt
cat /tmp/karukan-sample.txt
```

### 3. クラッシュログ確認

```bash
ls -lt ~/Library/Logs/DiagnosticReports/ | grep -iE 'karukan|KarukanIM' | head -5
```

---

## ログから原因を特定する

| ログに見えるもの | 原因 |
|---|---|
| `not initialized yet, consuming keyCode=...` が繰り返し出る | 仮説A: `karukan_session_init` ハング |
| `initialized = true` が出た後でも発生する | 仮説B or C |
| `panel was visible on activate` | 仮説B: パネルゾンビ（修正済みだが記録される） |
| `panel route keyCode=...` が繰り返し出る | 仮説B: パネルが hide されていない |
| クラッシュログに `KarukanIMExtension` の Rust panic | 仮説C |

---

## 追加したログ一覧（2026-03-14）

| 場所 | レベル | 内容 |
|---|---|---|
| `init` バックグラウンド処理 | `info` | `karukan_session_init: starting (thread=bg)` |
| `init` バックグラウンド処理 | `info` | `karukan_session_init: done ret=<値>` |
| `init` メインスレッド | `info` | `initialized = true` |
| `init` メインスレッド | `warning` | `self was deallocated before initialized=true`（self が先に解放された場合） |
| `handle(_:client:)` | `warning` | `not initialized yet, consuming keyCode=<値>` |
| `handle(_:client:)` パネルルート入口 | `debug` | `handle: panel route keyCode=<値> flags=<値>` |
| `activateServer` | `warning` | `panel was visible on activate — forcing hide` |
| `activateServer` | `info` | `activateServer initialized=<値>` |

---

## 回避策（再現時の応急処置）

1. **Escape を数回押す** — パネルゾンビ状態を解除できる場合がある
2. **入力ソースを一旦「英数」に切り替え、再度「KarukanIM」に戻す** — `deactivateServer` → `activateServer` が走り `panel.hide()` が実行される
3. **それでも直らない場合はプロセス再起動:**
   ```bash
   pkill -x KarukanIMExtension
   ```
   macOS が自動で再起動する（`LSBackgroundOnly` プロセスのため）
