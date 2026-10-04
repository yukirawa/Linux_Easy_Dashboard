# プラグイン — 自分でウィジェットを書く

**Linux_Easy_Dashboard** のウィジェットは、**TOML ファイル 1 枚**で追加できます。
再コンパイルも、共有ライブラリも、Web 技術も要りません。

```
~/.config/linux-easy-dashboard/plugins/cpu-freq.toml
```

置いてから **メニュー → プラグイン → プラグインを再読み込み** を選ぶと、追加ピッカー
（ヘッダーの `+`）に出てきます。あとは組み込みウィジェットと同じように置いて、
移動・リサイズ・削除できます。

例は [`examples/plugins/`](../examples/plugins) にあります。まとめて使うなら:

```sh
mkdir -p ~/.config/linux-easy-dashboard/plugins
cp examples/plugins/*.toml ~/.config/linux-easy-dashboard/plugins/
```

## できること

1 つのプラグインは「**どこから値を取るか**（`[source]`）」と「**どう見せるか**（`[view]`）」の
組です。**組み込みウィジェットも同じ TOML で書かれています**（`plugins/builtin/`）。
つまり「カーネル（土台とビューワー）」と「ウィジェット（定義）」が分かれており、
新しいウィジェットを足すのにカーネル側の変更は要りません。

## トップレベル

| キー | 型 | 既定 | 説明 |
| --- | --- | --- | --- |
| `id` | 文字列 | **必須** | `[a-z0-9_-]` のみ・64 文字まで。ファイル名にもなる |
| `name` | 文字列 | **必須** | ピッカーとタイルに出る名前 |
| `summary` | 文字列 | `""` | ピッカーの説明。タイル下部にも出る |
| `icon` | 文字列 | `application-x-executable-symbolic` | アイコンテーマの名前 |
| `category` | `system` \| `time` \| `info` \| `apps` | `info` | ピッカーの分類 |
| `hidden_when_idle` | 真偽 | `false` | **出力が空のときタイルごと隠す**（編集中は見える） |
| `size` | `[幅, 高さ]` | `[300, 180]` | 120〜1200 に丸められる |
| `aspect` | 数値 | なし | 縦横比（幅/高さ）。正方形は `1.0` |
| `refresh` | 整数（秒） | `5` | 1〜86400 |
| `author` / `version` / `homepage` / `license` | 文字列 | `""` | 公開用のメタデータ |

## `[source]` — 値の取り方

`[view]` が**ソース駆動**の場合は必須です。**自己完結型ビュー**（時計・モニタ・ランチャー等）
では省略できます（使っても無視されます）。

### `command`

```toml
[source]
kind = "command"
run = "printf 42"
timeout_secs = 5      # 既定 5、1〜600
```

`/bin/sh` で実行し、標準出力を読みます。末尾の空白は落ちます。

### `file`

```toml
[source]
kind = "file"
path = "/sys/class/thermal/thermal_zone0/temp"
```

最大 64 KiB 読みます。読めなければエラーになります。

### `clock`

```toml
[source]
kind = "clock"
format = "%H:%M:%S"
timezone = "Asia/Tokyo"   # 省略可（空でシステムのゾーン）
```

現在時刻を `strftime` で整形します。

### `http`

```toml
[source]
kind = "http"
url = "https://api.example.com/status"
timeout_secs = 10
json_pointer = "/current/temp"   # 省略可（RFC 6901）
```

GET します。`json_pointer` を付けると JSON の該当値だけを取り出します。

### 共通のふるまい

- 出力は 4096 バイトで切られます。
- **空の出力は「今は何も無い」**という意味で、エラーではありません
  （`hidden_when_idle` と組み合わせると「出番のないときは隠れる」ウィジェットになります）。
- コマンドが非ゼロ終了でも標準出力があればそれを使います。出力が無いときだけ
  エラーになり、その内容がタイルの説明行に出ます。
- パイプを使ったコマンドでは、終了ステータスは**最後のコマンド**のものです。

## `[view]` — 見せ方（必須）

`kind` で選びます。**ソース駆動**（値を `[source]` から取る）と
**自己完結型**（時計やモニタのように自分でデータを取る）があります。

### ソース駆動のビュー

| `kind` | 表示 | 追加のキー |
| --- | --- | --- |
| `readout` | 大きな値＋説明 | `label`（見出し）、`unit` |
| `bar` | 横バー | `unit` |
| `ring` | リングゲージ | `unit` |
| `sparkline` | 折れ線 | `history`（既定 60、2〜600）、`unit` |
| `list` | 行の一覧 | `rows`（既定 6、1〜64）、`split`（既定はタブ）、`label_column`（既定 0）、`value_column`（既定 1） |
| `facts` | キーと値の表 | `list` と同じ |
| `bars` | ラベル付きの横バー | `list` と同じ＋`filter`、`sort`（`"value"` / `"label"`）、`empty`、`unit` |
| `text` | 生のテキスト | `wrap`（既定 true） |

#### 数値の共通キー

`readout` / `bar` / `ring` / `sparkline` / `bars` は `[view]` の直下に次を書けます。

| キー | 既定 | 説明 |
| --- | --- | --- |
| `scale` | `1.0` | 数値に掛ける（例: kHz → MHz は `0.001`） |
| `offset` | `0.0` | 数値に足す |
| `decimals` | `0` | 小数点以下の桁数（最大 6） |
| `min` / `max` | なし | メーターの下限・上限。未指定なら 0〜100 |
| `warn_at` | なし | この値以上で警告色にする |

#### 値の読み方

出力の中から**最初に見つかった数値**を使います（`-?[0-9]+(\.[0-9]+)?`）。
`readout` は、出力が数値なら整形して表示し、数値でなければ**最初の行をそのまま**出します。

### 自己完結型のビュー

#### 時刻と日付

| `kind` | 表示 | キー |
| --- | --- | --- |
| `clock` | デジタル時刻 | `hour24`（既定 true）、`show_seconds`（false）、`show_date`（true）、`timezone`（IANA、空でシステム） |
| `analog_clock` | アナログ文字盤 | `show_seconds`（true）、`show_numerals`（false）、`smooth_seconds`（false） |
| `world_clock` | 複数タイムゾーン | `zones`（カンマ区切りの IANA）、`hour24`（true） |
| `calendar` | 今月のカレンダー | `week_starts_monday`（true） |
| `date` | 大きな日付 | `show_week`（true）、`show_progress`（true） |

#### 設定だけのビュー

| `kind` | 表示 | キー |
| --- | --- | --- |
| `heading` | 見出し＋アクセントバー | `title`、`subtitle`、`accent`（true） |
| `note` | メモ | `title`、`text` |
| `launcher` | アプリボタン | `apps`（desktop id のカンマ区切り）、`columns`（3）、`show_labels`（true） |

#### 情報

| `kind` | 表示 | キー |
| --- | --- | --- |
| `media` | MPRIS プレイヤー | `player`（空で自動）、`only_while_playing`（true）、`show_controls`（true）、`show_art`（true） |
| `weather` | 天気と予報 | `location`（都市名）、`fahrenheit`（false）、`days`（3） |

#### システムモニタ

| `kind` | 表示 | キー |
| --- | --- | --- |
| `cpu` | コア別＋推移 | `per_core`（true）、`show_history`（true） |
| `cores` | コアのヒートマップ | `columns`（0=自動）、`warn_by_usage`（true） |
| `memory` | メモリリング | `show_swap`（true）、`warn_by_usage`（true） |
| `disk` | マウント別リング | `mounts`（カンマ区切り、空でルート）、`max_rows`（4）、`show_free`（true） |
| `diskio` | ディスク I/O | `device`（空で自動）、`show_history`（true） |
| `network` | 送受信速度 | `interface`（空で自動）、`show_history`（true） |
| `load` | 負荷平均 | `show_history`（true） |
| `battery` | バッテリー | `show_time`（true）、`warn_by_charge`（true） |
| `processes` | プロセスランキング | `count`（6）、`by_cpu`（true） |
| `system` | 複合モニタ | `show_cpu` / `show_memory` / `show_load` / `show_disk`（true）、`disk_path`（`/`） |

## `[[settings]]` — 利用者に変えさせる

定義が許すなら、`[view]` の値を **タイルごとに** 変えられます。詳細ビューに出る行を宣言します。

```toml
[view]
kind = "bars"
rows = 4

[[settings]]
key = "rows"        # 上書きする [view] のキー
kind = "int"        # bool | int | text
title = "表示するセンサー数"
min = 1
max = 12
```

変えた値はそのタイルの `layout.json` にだけ保存され、定義ファイルは触りません。
値が形に合わないときは定義側の値がそのまま使われます。

## 組み込みウィジェットもプラグイン

アプリに最初から入っているウィジェットは、同じ TOML 形式で書かれたプラグインです。
`plugins/builtin/` にあり、バイナリに埋め込まれています。組み込み定義はユーザーの
`plugins/` より先に読み込まれ、書き換え・削除はできません（同じ `id` をユーザー側で
作っても組み込みが優先されます）。

## アプリ内での編集

プラグインのタイルをクリックすると詳細ビューが開き、そこに

- 現在の出力（生のまま）
- 定義ファイル（TOML）の**エディタ**と「保存」「削除」

があります。保存するとファイルが書き換わり、タイルはすぐ作り直されます。
定義を壊して保存しても**ファイルは保存され**、読み込みエラーが表示されるだけなので、
そのまま直せます。

## つまずきやすい点

- `id` に使えるのは `[a-z0-9_-]` だけです。大文字は使えません。
- **未知のキーや未知の `kind` があると、そのファイルは読み込まれません**。
  アプリのログ（`RUST_LOG=info`）に理由が出ます。
- `hidden_when_idle` のタイルは、編集モードにすると必ず見えます（消えたわけではありません）。
- 更新間隔は最短 1 秒です。秒未満のアニメーションはできません
  （`analog_clock` の `smooth_seconds` を除く）。

## セキュリティについて

`[source] kind = "command"` は、**あなたの権限で `/bin/sh` 経由で実行されます**。
他人が書いたプラグインをそのまま置かないでください。`sudo` を含むコマンドを書けば
`sudo` が要求されます（勝手に昇格はしません）。
`http` もあなたのネットワークから出ます。`file` はあなたが読める範囲でしか動きません。

## 動作の確認

```sh
# どのファイルが読み込まれているか
RUST_LOG=info linux-easy-dashboard 2>&1 | grep プラグイン
```

## カタログの構成

```
plugins/builtin/*.toml   組み込み（バイナリに埋め込み、変更不可）
examples/plugins/*.toml  サンプル
~/.config/linux-easy-dashboard/plugins/*.toml  あなたが書いた定義
```

カーネルが知っているのは「タイルの置き方」と「`[source]` / `[view]` の実行方法」だけです。
ウィジェットそのものはすべて TOML で定義されます。
