# Easy Dashboard Maker

Arch Linux 向けの、個人用デスクトップダッシュボード。
システム情報・時刻・気象・アプリ起動などを 1 つのウィンドウに集約し、
ウィジェットを自由に配置・リサイズ・カスタマイズできる。

- **完全ネイティブ**: GTK4 + libadwaita のみ。Web 技術（HTML/CSS/JS/WebView）は一切不使用。
- **単一バイナリ**: Flatpak / Snap を使わず、`cargo build --release` の成果物 1 つで動く。
- **Linux 準拠**: XDG ディレクトリ、freedesktop の `.desktop`、GNOME HIG に沿った UI。
- **拡張可能**: ウィジェットはレジストリに 1 エントリ足すだけで追加できる（現在 22 種類）。
  さらに**ユーザー自身が TOML 1 枚でウィジェットを書ける**（[プラグイン](docs/plugins.md)）。

## ビルドと実行

```sh
cargo build --release
./target/release/easy-dashboard-maker
```

必要なもの: Rust 1.85+、`gtk4`、`libadwaita`（Arch: `pacman -S gtk4 libadwaita`）。
天気ウィジェットは HTTPS のため OpenSSL（`openssl`）にリンクする。

インストールする場合:

```sh
install -Dm755 target/release/easy-dashboard-maker ~/.local/bin/easy-dashboard-maker
install -Dm644 data/io.github.yukirawa.EasyDashboardMaker.desktop \
  ~/.local/share/applications/io.github.yukirawa.EasyDashboardMaker.desktop
```

## 使い方

| 操作 | 動作 |
| --- | --- |
| ウィジェットをクリック | 詳細ビュー（設定はここで変更、即保存） |
| 詳細ビューのテキスト欄 | 入力が止まると自動保存（Enter で即保存） |
| ヘッダーの鉛筆ボタン / `Ctrl+E` | 編集モードの切り替え |
| 編集モードでドラッグ | 移動（離した瞬間にバネでスナップ） |
| 選択したタイルの右下グリップをドラッグ | リサイズ |
| 矢印キー / `Shift`+矢印 | 1px / グリッド単位で移動 |
| `Delete` | 選択中のウィジェットを削除 |
| 右クリック | 詳細表示・縦横比の固定・削除 |
| ヘッダーの `+` | ウィジェットを追加（ジャンル別・検索つき） |
| メニュー → プラグイン | 自分で書いた定義を再読み込み・フォルダを開く |

配置・設定・ウィンドウサイズは `$XDG_CONFIG_HOME/easy-dashboard-maker/layout.json` に
自動保存される（原子的書き込み、壊れたファイルは `layout.invalid.json` に退避）。
`$XDG_CONFIG_HOME/easy-dashboard-maker/style.css` を置くと見た目を上書きできる。

## ウィジェット

全部で 22 種。`+` の一覧はジャンルごとにまとまっていて、上の検索欄で名前・説明・
内部名から絞り込める（空になったジャンルは見出しごと消える）。
自分で書いたプラグインも同じ一覧の「プラグイン」欄に並ぶ。

| カテゴリ | ウィジェット | 内容 |
| --- | --- | --- |
| システム | CPU | コアごとの棒グラフ＋使用率の推移 |
| | コア | コアごとの使用率をセルのヒートマップで |
| | メモリ | 使用率のリングゲージ、スワップ |
| | ディスク | マウントごとのリング（自動選択 or 指定） |
| | ディスク I/O | 読み書きの速度と推移（一番忙しいディスクを追跡） |
| | ネットワーク | 送受信速度と推移（既定ルートを自動追跡） |
| | 負荷 | 負荷平均と推移（コア数で正規化） |
| | 温度 | hwmon のセンサーを棒グラフで |
| | バッテリー | 充電リング・状態・残り時間 |
| | プロセス | 重いプロセスをメモリ順 / CPU 順のランキングで |
| | システム | 上記をまとめた複合モニタ |
| 時刻と日付 | 時計 | デジタル時刻（24/12 時間、秒、日付） |
| | アナログ時計 | cairo 描画の文字盤（正方形・縦横比固定） |
| | ワールドクロック | 複数タイムゾーンと昼夜マーカー |
| | カレンダー | 今月（ロケール準拠、今日を強調） |
| | 今日 | 大きな日付と年の進み具合（ISO 週番号） |
| 情報 | 天気 | 現在の天気と数日予報（Open-Meteo） |
| | メディアプレイヤー | 再生中のトラックと操作（MPRIS/D-Bus、再生中だけ表示も可） |
| | システム情報 | ディストリビューション・カーネル・CPU・メモリ・稼働時間 |
| | 見出し | セクション名などのテキスト＋アクセントバー |
| | メモ | 短いテキスト（詳細ビューで編集） |
| アプリ | アプリランチャー | 固定したアプリを GIO 経由で起動 |

### ネットワークについて

天気ウィジェットだけが外部と通信する（`api.open-meteo.com` と
`geocoding-api.open-meteo.com`）。API キーもアカウントも不要で、場所の座標は
ウィジェット自身の設定に保存される。**このウィジェットを置かない限り、アプリは
一切通信しない。** 取得はワーカースレッド（`gio::spawn_blocking`）で行われ、
失敗しても画面は固まらず、その旨が表示されるだけ。

場所の名前を書き換えると、キャッシュした座標は捨てられて調べ直しになる。
遅れて届いた応答は「要求したときの設定がまだ有効か」を確かめてから反映するので、
古い地名の天気で新しい設定を上書きしてしまうことはない。

## プラグイン（自分でウィジェットを書く）

`~/.config/easy-dashboard-maker/plugins/*.toml` に置くだけで、自分だけのウィジェットを
追加できます（再コンパイル不要）。「どこから値を取るか」（コマンド / ファイル）と
「どう見せるか」（`readout` / `bar` / `ring` / `sparkline` / `list` / `text`）を宣言する
形式で、書式は [`docs/plugins.md`](docs/plugins.md)、動く例は
[`examples/plugins/`](examples/plugins) にあります。

```sh
mkdir -p ~/.config/easy-dashboard-maker/plugins
cp examples/plugins/*.toml ~/.config/easy-dashboard-maker/plugins/
```

置いたあと **メニュー → プラグイン → プラグインを再読み込み** で追加ピッカーに並びます。
プラグインの詳細ビューでは、現在の出力を見ながら**定義そのものを編集・保存・削除**できます。

## 構成

```
src/
├── main.rs            起動・ログ・panic フック
├── application.rs     AdwApplication、CSS 読み込み、GAction
├── window.rs          ウィンドウ、ヘッダーバー、保存デバウンス
├── config.rs          XDG 準拠の永続化（serde + serde_json）
├── graphics.rs        cairo 描画ヘルパとテーマ色の解決（Ink）
├── anim.rs            バネ（AdwSpringAnimation）ヘルパ
├── net.rs             HTTP（ワーカースレッド + メインコンテキストへ戻す）
├── canvas/            自作コンテナ
│   ├── geometry.rs    矩形とスナップ計算（GTK 非依存・テスト済み）
│   ├── imp.rs         GObject 実装（measure/allocate/snapshot、操作）
│   └── mod.rs         公開 API
├── widgets/           レジストリと各ウィジェット（22 種・カテゴリ順）
├── plugin.rs          ユーザー定義ウィジェット（TOML）の読み込み・実行
├── platform/          /proc・/sys の読み取り
├── ui/
│   ├── inspector.rs   詳細ダイアログ
│   └── debounce.rs    入力の合流（panic しない再アーム）
└── util.rs            表示用フォーマットと履歴バッファ
```

`docs/plugins.md` にプラグイン（ユーザー定義ウィジェット）のリファレンス、
`examples/plugins/` にそのまま使える例があります。

### 設計上の判断

- **キャンバスは自作の `gtk::Widget` サブクラス**。`GtkFixed` では移動・スナップ・グリップ・
  キー操作を後付けする必要があり、結局レイアウトを自前で持つことになるため。
  グリッド・ガイド線・グリップは `snapshot` で描くのでウィジェットツリーは軽いまま。
- **形は cairo、文字は GTK ラベル**。数値や見出しは本物のラベルなので、組版・
  アクセシビリティ・テーマがネイティブのまま。cairo はリング・折れ線・棒・天気記号を担当する。
- **テーマ色は CSS 経由で解決**（`graphics::Ink`）。cairo は色を知らず、`@accent_bg_color`
  などを CSS クラスから読み取るので、ライト/ダークやアクセント色の変更に追従する。
- **ウィジェットは `WidgetDescriptor` のレジストリ**。生成に失敗したら理由を表示する
  プレースホルダになるだけで、他へ波及しない。レジストリの並びがそのまま追加一覧の
  並びなので、**カテゴリごとに一続き**にしておく（崩れると見出しが飛び飛びになる。
  テストで固定してある）。
- **設定はウィジェット自身が持つ**。`WidgetContext` にパッチを送ると、キャンバスが
  *最新の* 設定へマージして保存する（詳細ビューを開いたままでも古い値で上書きしない）。
  同じ値のパッチは無視するので、タイルが無駄に再構築されない。
- **テキスト設定は入力しながら保存する**。`AdwEntryRow` の `apply` は apply ボタン
  （既定では非表示）専用なので、それを待つと入力が捨てられる。代わりに
  `changed` を短いデバウンスで合流させ、Enter では即座に保存する。
  デバウンスは発火時に自分の source を手放す（`glib::SourceId::remove` は
  発火済みの source を渡されると panic するため）。
- **Linux の情報は `/proc` と `/sys` から直接読む**。クロスプラットフォームな監視クレートは不要。
- **動きはバネ**。ドラッグ中は自由に動かし、離した瞬間にスナップ位置へ収束させる。
  `gtk-enable-animations`（GNOME の「視差効果を減らす」）が無効なら即時反映。
- **追加・移動・設定変更は同じ `changed` シグナルで伝える**。キャンバスはどれも
  「レイアウトが変わった」として通知するので、「ウィジェットがありません」の表示も
  自動保存も一つの経路で正しく更新される。
- **ユーザー定義ウィジェットは TOML の宣言にする**（共有ライブラリの `dlopen` ではない）。
  `dlopen` は `unsafe` と ABI 一致の要求を持ち込み、1 つのプラグインの異常がプロセス
  全体を巻き込む。代わりに「値の取り方（コマンド / ファイル）」と「見せ方（既存の
  ビュー）」の組を宣言させ、描画はアプリ側の検証済みコードが担当する。
  定義が壊れていてもそのタイルに理由が出るだけで、他へは波及しない。
- **タイルは自分を隠せる**。`WidgetContext::set_hidden` で、出すものが無いタイル
  （再生中のプレイヤーが無い、プラグインの出力が空）は自分を隠す。位置は保ったまま
  編集モードで必ず戻るので、見えなくなったウィジェットが行方不明にならない。

## 品質

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

実測値（release、Arch Linux + Wayland、この開発機）:

| 指標 | 実測 | 目標 |
| --- | --- | --- |
| 起動（プロセス開始 → ウィンドウ表示） | 418 ms | 500 ms 以内 |
| アイドル CPU（時計＋システムの 2 ウィジェット） | 0.25 % | 1 % 未満 |
| アイドル CPU（8 ウィジェット、うちゲージ 4 つ） | 数 % | 参考値 |
| メモリ (RSS) | 約 110–150 MB（うち約 80 MB は共有ライブラリ） | 100 MB 以下 |

メモリは GTK4 / libadwaita / Mesa の共有ページが大半で、アプリ固有部分は 30 MB 前後。
アイドル CPU は「静かなダッシュボード」なら 1 % 未満だが、毎秒描き直すゲージを
何枚も並べると数 % になる。この開発機はセッションが実験的な GPU ドライバで
動いている（MESA の警告）ため、ソフトウェア描画に落ちている可能性がある。
さらなる削減は cairo ではなく GSK の描画プリミティブで描くのが本筋（今後の課題）。

## 開発用フック

環境変数で、ヘッドレスに近い検証ができる（`src/devtools.rs`）。

```sh
# ウィジェット目録を 6 個ずつ並べて描画（ページ 0,1,2,3）
EDM_DEV_GALLERY=1 EDM_SCREENSHOT=/tmp/g.png RUST_LOG=info ./target/release/easy-dashboard-maker

# レイアウト・割り当て・詳細ビューの生成をログに出し、PNG に描画して終了
EDM_SCREENSHOT=/tmp/shot.png ./target/release/easy-dashboard-maker

# 編集モードにして (x,y) のタイルを (dx,dy) ドラッグ＝スナップの検証
EDM_DEV_DRAG="60,60,53,7" EDM_SCREENSHOT=/tmp/shot.png ./target/release/easy-dashboard-maker

# プラグインのタイルだけを並べる（目録には出ないため）
EDM_DEV_PLUGIN="cpu-freq,thermal" EDM_SCREENSHOT=/tmp/p.png RUST_LOG=info ./target/release/easy-dashboard-maker
```

## これから

1. 設定ウィンドウ（`AdwPreferencesWindow`）: グリッド幅・スナップ・テーマ・既定サイズ。
2. ウィジェット追加のドラッグ&ドロップ、キャンバス端での自動スクロール。
3. ゲージ描画を GSK の描画ノードに置き換えてアイドル CPU を下げる。
4. アプリアイコン（`data/` に追加して `Icon=` を実体に合わせる）。
5. 複数レイアウトのプロファイル切り替え、ウィジェットの複製。
6. プラグインに「アプリ内で新規作成」の導線（テンプレートから書き始める）。
