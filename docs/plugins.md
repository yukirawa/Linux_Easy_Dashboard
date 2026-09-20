# プラグイン — 自分でウィジェットを書く

Easy Dashboard Maker のウィジェットは、**TOML ファイル 1 枚**で追加できます。
再コンパイルも、共有ライブラリも、Web 技術も要りません。

```
~/.config/easy-dashboard-maker/plugins/cpu-freq.toml
```

置いてから **メニュー → プラグイン → プラグインを再読み込み** を選ぶと、追加ピッカー
（ヘッダーの `+`）の「プラグイン」欄に出てきます。あとは組み込みウィジェットと同じように
置いて、移動・リサイズ・削除できます。

例は [`examples/plugins/`](../examples/plugins) にあります。まとめて使うなら:

```sh
mkdir -p ~/.config/easy-dashboard-maker/plugins
cp examples/plugins/*.toml ~/.config/easy-dashboard-maker/plugins/
```

## 何ができるか

1 つのプラグインは「**どこから値を取るか**（`[source]`）」と「**どう見せるか**（`[view]`）」の
組です。値はコマンドの標準出力か、ファイルの中身です。数値として読めればメーター
（バー・リング・折れ線）にも使えます。

向いているもの:

- `/sys`・`/proc` の数値（温度・周波数・ファンなど）
- コマンド 1 行で取れるもの（更新件数、git の状態、プロセスの上位、`curl` した結果など）
- 決まった書式のテキスト（カーネル版、ログの 1 行、クリップボードの中身）

向いていないもの:

- 独自の図形を cairo で描きたい（組み込みウィジェットを作る領域です）
- 秒未満の更新（最小 `refresh = 1` 秒）
- 状態を持ちたい（プラグインは毎回ソースを読み直すだけです）

## 例

```toml
id = "cpu-freq"
name = "CPU 周波数"
summary = "現在の動作周波数 (MHz)"
icon = "power-profile-performance-symbolic"
category = "system"
size = [300, 220]
refresh = 2

[source]
kind = "file"
path = "/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"

[view]
kind = "ring"
min = 0
max = 5000
scale = 0.001
unit = "MHz"
label = "cpu0"
warn_at = 4000
```

```toml
id = "pacman-updates"
name = "更新"
summary = "pacman の更新件数"
hidden_when_idle = true

[source]
kind = "command"
run = "checkupdates 2>/dev/null | awk '$1 > 0 { print $1 }'"
timeout_secs = 60

[view]
kind = "readout"
label = "更新"
unit = "件"
```

## トップレベル

| キー | 型 | 既定 | 説明 |
| --- | --- | --- | --- |
| `id` | 文字列 | **必須** | `[a-z0-9-]` のみ・64 文字まで。ファイル名にもなる |
| `name` | 文字列 | **必須** | ピッカーとタイルに出る名前 |
| `summary` | 文字列 | `""` | ピッカーの説明。タイル下部にも出る |
| `icon` | 文字列 | `application-x-executable-symbolic` | アイコンテーマの名前 |
| `category` | `system` \| `time` \| `info` \| `apps` | `info` | 詳細ビューに出る分類 |
| `hidden_when_idle` | 真偽 | `false` | **出力が空のときタイルごと隠す**（編集中は見える） |
| `size` | `[幅, 高さ]` | `[300, 180]` | 120〜1200 に丸められる |
| `refresh` | 整数（秒） | `5` | 1〜86400 |

## `[source]` — 値の取り方（必須）

```toml
[source]
kind = "command"
run = "printf 42"
timeout_secs = 5      # 既定 5、1〜600
```

```toml
[source]
kind = "file"
path = "/sys/class/thermal/thermal_zone0/temp"
```

- `command` は `/bin/sh -c` で実行します。標準出力を読み、末尾の空白を落とします。
- `file` は最大 64 KiB 読みます。読めなければエラーになります。
- 出力は 4096 バイトで切られます（タイルに出す値なので、それ以上は意味がありません）。
- **空の出力は「今は何も無い」**という意味で、エラーではありません（`hidden_when_idle` と
  組み合わせると「出番のないときは隠れる」ウィジェットになります）。
- コマンドが非ゼロ終了しても標準出力があればそれを使います。出力が無いときだけ
  エラーになり、その内容がタイルの説明行に出ます。
- タイムアウトすると子プロセスを終了させます。

## `[view]` — 見せ方（必須）

`kind` で選びます。

| `kind` | 表示 | 追加のキー |
| --- | --- | --- |
| `readout` | 大きな値＋説明 | `label`（見出し） |
| `bar` | 横バー | — |
| `ring` | リングゲージ | — |
| `sparkline` | 折れ線 | `history`（既定 60、2〜600） |
| `list` | 行の一覧 | `rows`（既定 6、1〜64）、`split`（既定はタブ）、`label_column`（既定 0）、`value_column`（既定 1） |
| `text` | 生のテキスト | `wrap`（既定 true） |

### 数値の共通キー

`readout` / `bar` / `ring` / `sparkline` は `[view]` の直下に次を書けます。

| キー | 既定 | 説明 |
| --- | --- | --- |
| `scale` | `1.0` | 数値に掛ける（例: kHz → MHz は `0.001`） |
| `offset` | `0.0` | 数値に足す |
| `decimals` | `0` | 小数点以下の桁数（最大 6） |
| `min` / `max` | なし | メーターの下限・上限。未指定なら 0〜100 として扱う |
| `warn_at` | なし | この値以上で警告色にする |

### 値の読み方

- 出力の中から**最初に見つかった数値**を使います（`-?[0-9]+(\.[0-9]+)?`）。
  `48000`、`48.5°C`、`1.2 GiB` のような出力からも読み取れます。
- `readout` は、出力が数値なら整形して表示し、数値でなければ**最初の行をそのまま**出します。

### `list` の行の作り方

各行を `split` で区切り、`label_column` を名前、`value_column` を値にします。
列が足りない行は「行全体が名前・値なし」になります。

```toml
[source]
kind = "command"
run = "ps -eo rss=,comm= --sort=-rss | awk '{ printf \"%s|%.0f MB\n\", $2, $1/1024 }'"

[view]
kind = "list"
rows = 5
split = "|"
label_column = 0
value_column = 1
```

## アプリ内での編集

プラグインのタイルをクリックすると詳細ビューが開き、そこに

- 現在の出力（生のまま）
- 定義ファイル（TOML）の**エディタ**と「保存」「削除」

があります。保存するとファイルが書き換わり、タイルはすぐ作り直されます。
定義を壊して保存しても**ファイルは保存され**、読み込みエラーが表示されるだけなので、
そのまま直せます（他のプラグインやウィジェットには影響しません）。

## つまずきやすい点

- `id` に使えるのは `[a-z0-9-]` だけです。`_` や大文字は使えません。
- **未知のキーや未知の `kind` があると、そのファイルは読み込まれません**。
  アプリのログ（`RUST_LOG=info`）に理由が出ます。
- パイプを使ったコマンドでは、終了ステータスは**最後のコマンド**のものです。
  `git ... | awk ...` は `git` が失敗しても「空出力」になるので、タイルは隠れます。
  その場合は詳細ビューの「現在の出力」を見てください。
- `hidden_when_idle` のタイルは、編集モードにすると必ず見えます（消えたわけではありません）。
- 更新間隔は最短 1 秒です。秒未満のアニメーションはできません。

## セキュリティについて

`[source] kind = "command"` は、**あなたの権限で `/bin/sh` 経由で実行されます**。
他人が書いたプラグインをそのまま置かないでください。`sudo` を含むコマンドを書けば
`sudo` が要求されます（勝手に昇格はしません）。
ファイルの読み取りも同様に、あなたが読める範囲でしか動きません。

## 動作の確認

```sh
# どのファイルが読み込まれているか
RUST_LOG=info easy-dashboard-maker 2>&1 | grep プラグイン
```
