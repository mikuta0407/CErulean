# 開発・CLI の使い方

[README に戻る](../README.md)。コマンドはリポジトリ直下で実行します。

## ビルドと実行

CLI だけなら `cargo build --release -p cerulean-cli` でビルドできます。このビルドは Web 資材を
埋め込まないため、単体配布には `tools/build.sh` を使ってください。
手動では `tools/web-build.sh` の後に `cargo build --release -p cerulean-cli --features embedded-web` でも作れます。

必要なツール: rustup（`rust-toolchain.toml` で固定した Rust の版）。wasm のビルドと
`tools/check.sh` には wasm-bindgen-cli（`Cargo.toml` の wasm-bindgen と同じ版）と
Node.js も要る。

```sh
cargo build --release
R=target/release/cerulean

# イメージの情報表示（開始アドレス・長さ・エントリポイント・レコード）
$R info tmp/images/PPC_USA.bin

# 実行（カーネルのデバッグシリアル UART1 の出力が標準出力に流れる。未実装の
# 命令等に当たると PC と直前の命令履歴を表示して停止する）
$R run --rtc 2006-01-02T15:04:05 tmp/images/PPC_USA.bin

# トレース（逆アセンブル付き）・命令数の上限
$R run --trace --max-steps 1000 tmp/images/PPC_USA.bin

# Today 画面まで起動（36 億命令、約 45 秒）して画面を PNG に保存。
# 1 億命令ごとの連番 PNG は --fb-every 100000000 を足す
$R run --rtc 2006-01-02T15:04:05 --max-steps 3600000000 --fb-out screen.png tmp/images/PPC_USA.bin

# ゲストの音を WAV に書く（音の間は仮想時間に合わせて無音で埋める。起動音は約 16.4 秒目）
$R run --rtc 2006-01-02T15:04:05 --max-steps 3600000000 --audio-out boot.wav tmp/images/PPC_USA.bin

# 物理アドレス範囲へのアクセスを表示（周辺機器の調査用。1 命令ずつ進むので遅い）
$R run --watch 0x4D000000-0x4D000FFF --max-steps 100000000 tmp/images/PPC_USA.bin
```

`--rtc` を省くとホストの現在時刻（UTC）になる。固定すると実行が完全に再現可能になる。
全フラグは `$R`（引数なし）で表示される。

### 入力スクリプトとスナップショット

```sh
# 一度だけ: Today 画面まで起動してスナップショットを保存（無圧縮で約 134MB）
$R run --rtc 2006-01-02T15:04:05 --max-steps 3600000001 \
  --snap-save today.snap@3600000000i tmp/images/PPC_USA.bin

# 以後はスナップショットから再開し、スクリプトで操作する（イメージを渡すと照合する）
$R run --snap-load today.snap --script calendar.txt

# スナップショットのチャンクの一覧・2 つの比較
$R snapdump today.snap
```

`calendar.txt` の例（`@` は絶対時刻、`+` は直前のコマンドの終了からの相対時刻。
単位は `s`・`ms`・命令数 `i`。仮想時刻は命令数から決まるので、何度実行しても
同じ結果になる）:

```
@3600100000i tap 20 10        # Start（座標は 240x320 の画面ピクセル）
+1s          tap 50 52        # Calendar
+2s          press Right      # キー（Up/Down/Left/Right/Enter/A〜Z/0〜9/App1〜5 など）
+2s          shot cal.png     # 画面を PNG に保存
+0i          snap cal.snap    # その時点のスナップショット
+0i          quit
```

コマンドは `tap x y [押下時間]`・`down x y`・`move x y`・`up`・`key down|up 名前`・
`press 名前 [押下時間]`・`card insert ファイル`・`card eject [ファイル]`・`nic insert [MAC]`・
`nic eject`・`net rx 16進`・`shot ファイル`・`snap ファイル`・`quit`。書式の詳細は `core/src/script.rs` の先頭コメントを参照。

### ストレージカード

PC カードのソケットに CompactFlash を挿すと、WM5 からは「Storage Card」に見えます
（設計と根拠は `docs/storage-card-design.md`）。中身は MBR＋FAT のディスクイメージで、
ホストからファイルを出し入れできます。

```sh
# ホストのフォルダの中身を入れた 64MB のカードを作る（8M〜512M）
$R card new card.img --size 64M --from ~/wm5-files
$R card ls card.img                       # 一覧（card ls card.img "My Photos" でフォルダの中）
$R card put card.img photo.jpg --to "My Photos"   # ファイル・フォルダを入れる
$R card get card.img "My Photos" out/     # 取り出す
$R card rm card.img photo.jpg             # 消す

# 起動時に挿し、止めたときに（ゲストが書いた内容を含む）イメージを書き出す
$R run --snap-load today.snap --card card.img --card-out card-after.img
```

スクリプトでは `card insert card.img` で動作中に挿し（ホットプラグ）、`card eject out.img` で
抜いて中身を書き出します。

ネットワーク（下記）と同時に使うときは、同じカードのイメージを Device Emulator のフォルダ共有の
方式で挿します（PC カードのソケットを使わない。WM5 からは同じ「Storage Card」。設計は
`docs/folder-share-design.md`）: `run --share card.img --share-out after.img`、スクリプトでは
`share insert card.img`・`share eject out.img`。ブラウザ版はネットワークがオンの間に挿すと自動で
この方式になります。ブラウザ版はメニューの「ストレージカード」で、カードを作る・
ファイルを入れる（抜いている間）・挿す・抜くができます。

### ネットワーク（インターネット）

PC カードのソケットに NE2000 互換のイーサネットカードを挿すと、WM5 が DHCP でアドレスを
取り、Internet Explorer で http のページを開けます（設計と根拠は `docs/network-design.md`）。
ゲストの TCP はエミュレータの外側の小さな NAT（`net`）で終端し、外へは TCP の
バイト列だけを出します。**初めて使うときは WM5 の「設定 → 接続 → ネットワークカード」で
「ネットワークカードの接続先」を「インターネット設定」にしてください**（スナップショットに
残ります）。

HTTPS のサイトも開けます: IE Mobile の古い TLS はエミュレータの外側（`net`）で受け、外へは
中継サーバー・CLI が今の TLS でつなぎ直します。最初に一度、WM5 の IE で `http://10.0.2.2/` を開き、
証明書（CErulean Local CA）を開いてインストールしてください（CLI は `--net-ca ca.bin` で CA を作る・使う。
ブラウザ版は端末の中で作る）。

```sh
# CLI: OS のソケットで直接つなぐ（実時間に合わせて進む）。受け取ったフレームを記録し、
# 送受信を pcap に書く
$R run --snap-load today.snap --nic --net --net-ca ca.bin --net-record net.txt --net-pcap net.pcap --script ops.txt
# 記録の再生（ネットワークなしで同じ状態になる）
$R run --snap-load today.snap --nic --script ops.txt --script net.txt

# ブラウザ版の中継サーバーだけを動かす（トークンを省くと乱数で作って表示する。
# アプリの配信と一緒なら cerulean serve --with-relay）
$R relay --listen 127.0.0.1:8765
```

ブラウザ版はメニューの「ネットワーク（中継サーバー経由）」で中継サーバーの URL（既定は配信元の
`/relay`。`cerulean relay` を別に動かすなら `ws://127.0.0.1:8765/`）とトークンを入れてオンにします。オンにしたときだけ、その中継
サーバーとだけ通信します（オフの間、サイトは外部と通信しません）。https で配信したページ
からは `wss://` の中継サーバー（TLS は前段のリバースプロキシで付ける）か、自分の端末の
`ws://127.0.0.1` にしかつなげません。オンの間にストレージカードを挿すと、PC カードのソケットを
使わないフォルダ共有の方式になり、同時に使えます。

## 開発

```sh
# コミット前の確認（fmt・clippy・テスト〔ネイティブと wasm32〕・wasm のビルド）
tools/check.sh
# 単体配布の確認（Chrome 必要。リポジトリ外での起動・静的配信・オフライン再読込）
bash tools/build.sh
node tools/browser/app-distribution.mjs target/release/cerulean
# コアの動作に関わる変更では、全基準シナリオの照合も（約 4 分）
CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh
```

設計方針は [CLAUDE.md](../CLAUDE.md)、設計文書の案内は [README.md](README.md) を参照。

### 一致確認

`testdata/golden/` に基準シナリオ（リセット起点。イメージ＋固定の RTC＋絶対命令数の
スクリプト）と期待値（CPU 状態・RAM・UART1・画面のハッシュ）がある。定義は
[testdata/golden/README.md](../testdata/golden/README.md)。コアの動作を意図して変えたときは
`tools/golden/regen.sh` で作り直す。

```sh
# run の一致確認用フラグ
$R run --rtc 2006-01-02T15:04:05 --max-steps 1200000000 \
  --result r.jsonl --checkpoint 400000000 \
  --trace-hash 10000000 --trace-hash-ram 100000000 --trace-hash-out th.txt tmp/images/PPC_USA.bin
$R goldencmp testdata/golden/expected/boot-1200M.jsonl r.jsonl
```

### 調査・計測の道具

`tools/re/` の `rommods.py` は ROM のモジュール一覧、`pcsample.py` は PC サンプルの
モジュール別集計、`pcicwatch.py` は PC カードの監視ログの整形に使います。

| 道具 | 用途 |
|---|---|
| `tools/bench/bench.sh` | 基準のリビジョン（既定 HEAD）と作業ツリーの速度を交互に計測 |
| `$R segspeed <snap> <script>` | スクリプト再生中の仮想 0.25 秒ごとの実時間比・アイドル割合 |
| `$R ihist [--pairs] <snap>` | 実行した ARM 命令の種類（--pairs は連続 2 命令の組）の分布 |
| `$R genrate <image>` | MMU の変換世代・コードページの印付けの頻度・デコード済みページ数 |
| `$R goldencmp <a.jsonl> <b.jsonl>` | 一致確認の結果の比較（レジスタ単位で差を表示） |
| `$R snapdump <snap> [snap2]` | スナップショットのチャンクの一覧・比較 |

## 構成

Cargo ワークスペースはリポジトリ直下です。`cargo` コマンドも直下で実行します。

| 場所 | 役割 |
|---|---|
| `Cargo.toml` / `Cargo.lock` / `rust-toolchain.toml` | ワークスペースと依存・Rust の版の固定 |
| `core` | エミュレータのコア（`cerulean-core`。std のみ・プラットフォーム非依存） |
| `core/src/arm` | ARMv4T インタプリタ（デコードキャッシュ・特化・アイドル検出・逆アセンブラ） |
| `core/src/mmu.rs` | MMU / CP15（テーブルウォーク・権限・FCSE・ソフト TLB） |
| `core/src/bus.rs` | 物理アドレス空間（RAM と MMIO の振り分け・監視） |
| `core/src/s3c2410` | S3C2410 周辺機器（UART・割り込み・タイマー・LCD・RTC・ADC/タッチ・SPI など） |
| `core/src/smdk2410` | ボード構成・実行ループ・入力 API・スナップショットの並び |
| `core/src/{snapshot,script,emu,loader}.rs` | 保存形式・入力スクリプト・イベントの適用・イメージローダー |
| `cli` | ネイティブの開発用 CLI（`cerulean`） |
| `web` | ブラウザ版（wasm）のエントリ |
| `tools/` | 一致確認・計測・確認のスクリプト |
| `testdata/golden/` | 一致確認の基準（シナリオ・期待値・合成プログラム） |
| `docs/` | イメージの取得手順・各機能の設計・開発記録 |


## ブラウザの自動テスト

Chrome での確認（CDP で操作する。依存なし）:
- `node tools/browser/app-e2e.mjs tmp/images/PPC_USA.bin tmp/app-e2e` — 起動・自動保存・
  記録と操作・書き出し・リロードと強制終了からの再開・タブの排他・CLI での再生の一致
  （約 2 分。先に `cargo build --release`）
- `node tools/browser/app-smoke.mjs tmp/images/PPC_USA.bin tmp/app-smoke` — 操作と画面の PNG
  （`--desktop`・`--viewport=844x390`・`--layout-only`・`--headed`）
- `node tools/browser/app-audio.mjs tmp/images/PPC_USA.bin tmp/app-audio` — 音をオンにして等速で
  起動し、起動音が AudioContext に渡ることを確かめる（約 30 秒）
- `node tools/browser/app-profile.mjs tmp/images/PPC_USA.bin tmp/images/wm6/PPC_JPN_GSM_VGA.bin tmp/app-profile`
  — プロファイルの作成・別のイメージ（VGA）の起動・切り替えて続きから再開・リロード・削除（約 3 分）
