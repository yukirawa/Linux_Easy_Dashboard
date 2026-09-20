# Easy Dashboard Maker v0.1.0

Arch Linux 向けの、**Web 技術を一切使わない**ネイティブなデスクトップダッシュボードです。
Rust + GTK4 + libadwaita だけで書かれており、単一バイナリで動きます。

## ハイライト

- **22 種のウィジェット** — システム (CPU・コア/メモリ・ディスク・ディスク I/O・ネットワーク・
  負荷・温度・バッテリー・プロセス・複合)、時刻と日付 (時計・アナログ時計・世界時計・
  カレンダー・今日)、情報 (天気・メディアプレイヤー・システム情報・見出し・メモ)、
  アプリ (ランチャー)
- **ウィジェットを自分で書ける** — `~/.config/easy-dashboard-maker/plugins/*.toml` に
  「どこから値を取るか」「どう見せるか」を宣言するだけで、再コンパイルなしに追加できます。
  アプリ内の詳細ビューで定義をそのまま編集・保存・削除もできます（[書式](docs/plugins.md)）
- **メディアプレイヤー** — MPRIS (D-Bus) 経由で再生中のトラックを表示し、再生/一時停止/
  スキップを操作。再生が無いときはタイルごと隠れます
- **自由な配置** — ドラッグで移動、グリップでリサイズ、グリッド/ガイドへのスナップ、
  縦横比の固定。離した瞬間にバネで収束
- **XDG 準拠** — 設定は `$XDG_CONFIG_HOME/easy-dashboard-maker/` に、テーマは
  `style.css` で上書きできます

## インストール

```sh
tar -xzf easy-dashboard-maker-0.1.0-x86_64-linux.tar.gz
cd easy-dashboard-maker-0.1.0
install -Dm755 easy-dashboard-maker ~/.local/bin/easy-dashboard-maker
install -Dm644 data/io.github.yukirawa.EasyDashboardMaker.desktop \
  ~/.local/share/applications/io.github.yukirawa.EasyDashboardMaker.desktop
easy-dashboard-maker
```

必要なもの: `gtk4`、`libadwaita`、`openssl`（天気ウィジェットの HTTPS 用）。
Arch なら `pacman -S gtk4 libadwaita openssl`。

## 品質

- `cargo fmt` / `cargo clippy --all-targets -- -D warnings` / 88 テスト
- 起動 418 ms（目標 500 ms 以内）、アイドル CPU 0.25 %（目標 1 % 未満）
- `unsafe` なし。実行時 panic を避け、ウィジェットの失敗はそのタイル内に留まる

## 既知の制限

- 起動時とウィンドウ表示は Arch Linux + Wayland で検証しています。
- メディアプレイヤーは MPRIS 対応のプレイヤーが動いているときだけ内容を表示します。
  Spotify（`org.mpris.MediaPlayer2.spotify`）で再生中の表示・進捗の追従を検証済みです。
  操作ボタン（再生/一時停止/スキップ）は同じ呼び出し経路を使いますが、クリックでの確認は
  まだ人手での確認が必要です。
- プラグインは「宣言」型です。独自の図形を cairo で描きたい場合は組み込みウィジェットを
  追加してください。
