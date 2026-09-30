# CErulean

Windows Mobile 5.0（Windows CE 5.0 ベース）の LLE エミュレータ。Rust 製（2026-09 に
Go から移行）。

Microsoft Device Emulator 向けの WM5 エミュレータイメージ（英語版・日本語版）を、
Samsung S3C2410（ARM920T）構成のマシンで動かす。最初の出荷先はブラウザ（wasm。PC と
iOS/iPadOS の Safari）で、将来はネイティブ（iOS/Android/PC）と JIT にも対応する予定。
将来的には PXA27x 系の実機構成の追加も予定。

## 状態

マイルストーン1〜5 は Go 版での記録（フラグ名は Go 版のもの）。

マイルストーン1（完了）: ローダー・ARM インタプリタの骨組み・バス/UART・CLI。

マイルストーン2（完了）: 実イメージのブートがカーネルのデバッグシリアル出力まで到達。

- [x] ARMv4T 命令セットの完成（乗算・SWP・ユーザーバンク LDM/STM・Thumb 一式）
- [x] MMU（変換テーブルウォーク・ドメイン・アクセス権限・FCSE・high vectors）
- [x] 例外配送（データ/プリフェッチアボート・未定義命令・IRQ/FIQ）
- [x] S3C2410 周辺機器: 割り込みコントローラと PWM タイマーは実動作
      （命令数ベースの仮想時間）、GPIO・クロック等は値保持スタブ
- [x] デバッグ支援: `-trace` の簡易ディスアセンブラ、停止時の直前命令履歴表示

マイルストーン3（完了）: 実イメージが **Today 画面まで起動**し、LCD の
フレームバッファを PNG で確認できる。

- [x] LCD コントローラ（LCDCON/LCDSADDR からフレームバッファの位置・解像度・bpp を解釈）
- [x] フレームバッファの PNG 出力（停止時 `-fb-out`、一定間隔 `-fb-every`）
- [x] RTC（仮想時間で進む。初期時刻は `-rtc` で固定可能）
- [x] 性能改善: MMU のソフト TLB と物理バスの O(1) 領域検索（約 10M → 22M 命令/秒）
- [x] デバッグ支援: 物理アドレス監視 `-watch`、`-trace-from`、`-sample`、
      Thumb 命令のディスアセンブル、停止時の PC の VA→PA 表示

Today 画面の完成まで約 35 億命令（マイルストーン3 時点の速度で約 2.5 分。現在は約 49 秒）かかる。

マイルストーン4（完了）: 入力を与えて UI を操作できる。スクリプトで
「Start をタップ → Calendar / Settings を起動」を再現し PNG で確認できる。

- [x] スナップショット（全状態の保存・復元）。Today 画面から数秒で再開できる
- [x] 決定論的な入力スクリプト（仮想時刻付きのタップ・キー・画面保存）
- [x] タッチパネル（S3C2410 ADC/タッチスクリーン I/F、touch.dll の変換式に合わせた座標変換）
- [x] ハードウェアキー（SPI1 のキーボード用マイコン）: 方向キー・Enter・英数字・App ボタン
- [ ] ソフトキー（VK_F1/F2）の物理キー入力（ドライバの表に無い。画面のソフトキー表示のタップで代用可）
- [ ] 電源ボタン（pwrbtn2410.dll、EINT0）

マイルストーン5（進行中）: ブラウザから対話的に操作できる。

- [x] 性能改善: アイドルスキップ（割り込み待ちのスピン・ADC 変換待ちを、状態を
      変えずに次のイベント直前まで飛ばす）・デバイス時間のまとめ進め・デコード
      キャッシュ・ブロック単位の実行・頻出命令の特化・LDM/STM の高速化。
      実処理の区間は約 21M → 75〜80M 命令/秒（実時間の約 0.55 倍）、アイドルは
      実時間の数十倍まで出せる（serve は等速に合わせて待つ）。Today 完成まで
      約 2.5 分 → 約 49 秒。変更前後で出力・スナップショットが完全一致
- [x] 実行 API（`machine.Machine` の RunUntil・入力・画面・スナップショット、
      `emu` パッケージのイベント照合と記録）
- [x] ブラウザ型フロントエンド `cerulean serve`（画面・タッチ・キー、等速/最高速、
      一時停止、スナップショット保存）
- [x] 操作の記録と再生（記録を絶対命令数のスクリプトに書き出し、`run` で同じ画面を再現）

Rust 移行（2026-09〜）: コアを Rust に移し、最初はブラウザ（wasm）で
動かす。計画は [docs/rust-migration-plan.md](docs/rust-migration-plan.md)。

- [x] 段階0（準備）: Go 版に一致確認の道具（`-result`・`-checkpoint`・`-trace-hash`）、
      基準シナリオと期待値（`testdata/golden/`）、Rust のワークスペースの骨組み（`rust/`）
- [x] 段階1: コアの移植（インタプリタ。ネイティブで Go 版と完全一致）。全基準シナリオで
      Go と一致し、Go の高速化（デコードキャッシュ・特化・アイドルスキップ等）と
      新しいスナップショット形式も移した。速度は Go と同等（実処理の区間で約 5% 速い）。
      基準を Rust 版に切り替え、Go 版を削除した（最後の Go 版のコミットは 039ccb4）
- [x] 段階2: wasm で動かして計測（Node・Firefox・Chrome/Edge・Safari・iPhone で全基準
      シナリオ一致。起動の区間でブラウザ 84〜133M 命令/秒）
- [x] 段階4（4-1・4-2 で区切り）: ページ内のブロック実行、8 バイトの IR と特化の拡大。
      起動の区間でネイティブ 81→約 100M、Node の wasm 47→70M 命令/秒
- [ ] 段階5: JIT-to-wasm（進行中。設計は `docs/stage5-design.md`）
  - [x] 5-1: 最小の JIT（単純な特化命令のブロック、扱えない命令の手前でインタプリタに
        戻る）。全基準シナリオで JIT ありでも完全一致。速度はまだインタプリタ以下
        （ブロックごとに JS を往復するため）
  - [x] 5-2: ページ単位の関数とブロックの連結、対象の拡大（生成コードで約 96% を実行）、
        関数テーブルでの呼び出し。Node で JIT なしの約 2 倍（起動 4 億命令で約 155M 命令/秒）
  - [x] 5-4 前半: V8 のメモリ（TLB の確認を補助関数に出し、関数の大きさに上限）。
        途中の最大の RSS が JIT なし +約 700MB → +約 100MB、JIT ありは約 173M 命令/秒
  - [x] 5-3: ページをまたぐ連結（Today まで Node で約 105M 命令/秒）
  - [ ] 5-4 後半: 各ブラウザでの計測・コード量の上限（Chrome 378M・iPad Safari 約 570M
        命令/秒は確認済み）
- [ ] 段階3: ブラウザ版の最小製品
  - [x] Worker＋画面表示（実時間との同期・起動は Today まで早送り）、入力（タッチ・
        画面外のハードウェアボタン・PC のキー）、スマートフォンの縦横・PC の配置
  - [x] 保存（OPFS。イメージ・スナップショットの保存・読み込み・書き出し）、自動保存と
        再開（リロード・強制終了から）、1 タブだけ動かす、記録と書き出し（CLI で再生して
        一致）、PWA 化。Chrome で確認
  - [ ] Firefox・Safari・iOS Safari での確認

## テスト用イメージの入手

動作確認には Microsoft Device Emulator 用の Windows Mobile 5.0 イメージが必要になる
（リポジトリには含まれない）。以下の手順で `tmp/images/` に取得できる。

```sh
mkdir -p tmp/images && cd tmp/images

# Windows Mobile 5.0 Pocket PC SDK (182MB) を archive.org から取得
curl -L -o wm5sdk.msi \
  "https://archive.org/download/windows-mobile-5.0-pocket-pc-sdk_202305/Windows%20Mobile%205.0%20Pocket%20PC%20SDK.msi"

# .msi 内のエミュレータイメージ PPC_USA.bin を抽出（要 cabextract）
cabextract -F '_208PPC_USA_bin' wm5sdk.msi
mv _208PPC_USA_bin PPC_USA.bin
rm wm5sdk.msi  # 抽出後は不要

# 確認（B000FF 形式、start=80070000 / entry=80076CF0 / 99 レコードのはず）
cd ../.. && rust/target/release/cerulean info tmp/images/PPC_USA.bin
```

## ブラウザ版（段階3・作業中）

```sh
tools/web-build.sh          # wasm をビルドして rust/web/www/pkg に置く
tools/serve-bench.py 8000   # 配信（キャッシュ無効）
# http://localhost:8000/rust/web/www/app/ を開き、PPC_USA.bin を選ぶ
```

イメージを選ぶと Today まで最高速で起動し（Chrome の JIT ありで数十秒）、着いたら自動で
保存して、以後は等速で動く。画面のタップ（マウス・タッチ）、画面の下（横向きでは右）の
ボタン（方向キー・決定・App1〜5）、PC のキー（矢印・Enter・英数字・F1〜F5 = App1〜5 など。
IME は切る）で操作する。

- 保存: イメージとスナップショットは端末の OPFS に置く（外部に送らない）。自動保存は
  3 世代（Today 到着時・画面を離れたとき・操作があれば 30 秒ごと・なければ 5 分ごと）。
  次に開くと「続きから再開」が出る。メニューから手動保存・書き出し（.snap.gz）・読み込み。
  **書き出したスナップショットにはイメージの中身が入る**ので公開の場で共有しないこと。
- 記録: メニューの「記録開始」で起点スナップショットを取り、「記録停止」で入力の
  スクリプト（絶対命令数）を作る。書き出した 2 つを CLI で再生すると同じ状態になる
  （スクリプトの先頭のコメントに手順と、終わりの CPU・RAM・画面の SHA-256）:
  `gunzip X.snap.gz && cerulean run --snap-load X.snap --script X.txt --result r.jsonl PPC_USA.bin`
- 時計: 保存から再開したとき・画面に戻ったとき・起動の早送りの後に、ゲストの時計を端末の
  時刻に合わせる（メニューで切れる。記録にも `rtc` コマンドとして残るので再生しても同じ）。
- 動かせるのは 1 タブだけ（2 つ目のタブは待つ）。ホーム画面に追加でき、オフラインでも開ける。

Chrome での確認（CDP で操作する。依存なし）:
- `node tools/browser/app-e2e.mjs tmp/images/PPC_USA.bin tmp/app-e2e` — 起動・自動保存・
  記録と操作・書き出し・リロードと強制終了からの再開・タブの排他・CLI での再生の一致
  （約 2 分。先に `cd rust && cargo build --release`）
- `node tools/browser/app-smoke.mjs tmp/images/PPC_USA.bin tmp/app-smoke` — 操作と画面の PNG
  （`--desktop`・`--viewport=844x390`・`--layout-only`・`--headed`）

## ビルドと実行

必要なツール: rustup（`rust/rust-toolchain.toml` の版が自動で入る）。wasm のビルドと
`tools/check.sh` には wasm-bindgen-cli（`rust/Cargo.toml` の wasm-bindgen と同じ版）と
Node.js も要る。

```sh
cd rust && cargo build --release && cd ..
R=rust/target/release/cerulean

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
`nic eject`・`net rx 16進`・`shot ファイル`・`snap ファイル`・`quit`。書式の詳細は `rust/core/src/script.rs` の先頭コメントを参照。

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
抜いて中身を書き出します。ブラウザ版はメニューの「ストレージカード」で、カードを作る・
ファイルを入れる（抜いている間）・挿す・抜くができます。

### ネットワーク（インターネット）

PC カードのソケットに NE2000 互換のイーサネットカードを挿すと、WM5 が DHCP でアドレスを
取り、Internet Explorer で http のページを開けます（設計と根拠は `docs/network-design.md`）。
ゲストの TCP はエミュレータの外側の小さな NAT（`rust/net`）で終端し、外へは TCP の
バイト列だけを出します。**初めて使うときは WM5 の「設定 → 接続 → ネットワークカード」で
「ネットワークカードの接続先」を「インターネット設定」にしてください**（スナップショットに
残ります）。

HTTPS のサイトも開けます: IE Mobile の古い TLS はエミュレータの外側（`rust/net`）で受け、外へは
中継サーバー・CLI が今の TLS でつなぎ直します。最初に一度、WM5 の IE で `http://10.0.2.2/` を開き、
証明書（CErulean Local CA）を開いてインストールしてください（CLI は `--net-ca ca.bin` で CA を作る・使う。
ブラウザ版は端末の中で作る）。

```sh
# CLI: OS のソケットで直接つなぐ（実時間に合わせて進む）。受け取ったフレームを記録し、
# 送受信を pcap に書く
$R run --snap-load today.snap --nic --net --net-ca ca.bin --net-record net.txt --net-pcap net.pcap --script ops.txt
# 記録の再生（ネットワークなしで同じ状態になる）
$R run --snap-load today.snap --nic --script ops.txt --script net.txt

# ブラウザ版の中継サーバー（トークンを省くと乱数で作って表示する）
$R relay --listen 127.0.0.1:8765
```

ブラウザ版はメニューの「ネットワーク（中継サーバー経由）」で中継サーバーの URL
（`ws://127.0.0.1:8765/`）とトークンを入れてオンにします。オンにしたときだけ、その中継
サーバーとだけ通信します（オフの間、サイトは外部と通信しません）。https で配信したページ
からは `wss://` の中継サーバー（TLS は前段のリバースプロキシで付ける）か、自分の端末の
`ws://127.0.0.1` にしかつなげません。ソケットは 1 つなので、ストレージカードとは同時に
使えません。

## 開発

```sh
# コミット前の確認（fmt・clippy・テスト〔ネイティブと wasm32〕・wasm のビルド）
tools/check.sh
# コアの動作に関わる変更では、全基準シナリオの照合も（約 4 分）
CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh
```

設計方針は [CLAUDE.md](CLAUDE.md) を参照。

### 一致確認

`testdata/golden/` に基準シナリオ（リセット起点。イメージ＋固定の RTC＋絶対命令数の
スクリプト）と期待値（CPU 状態・RAM・UART1・画面のハッシュ）がある。定義は
[testdata/golden/README.md](testdata/golden/README.md)。コアの動作を意図して変えたときは
`tools/golden/regen.sh` で作り直す。

```sh
# run の一致確認用フラグ
$R run --rtc 2006-01-02T15:04:05 --max-steps 1200000000 \
  --result r.jsonl --checkpoint 400000000 \
  --trace-hash 10000000 --trace-hash-ram 100000000 --trace-hash-out th.txt tmp/images/PPC_USA.bin
$R goldencmp testdata/golden/expected/boot-1200M.jsonl r.jsonl
```

### 調査・計測の道具

ゲストのドライバの調査用のスクリプト（`tools/re/`: ROM のモジュール一覧・PC のサンプリングの
モジュール別の時系列・PC カードの監視ログの整形）の使い方は `docs/network-kickoff.md` の「調べ方のメモ」。

| 道具 | 用途 |
|---|---|
| `tools/bench/bench.sh` | 基準のリビジョン（既定 HEAD）と作業ツリーの速度を交互に計測 |
| `$R segspeed <snap> <script>` | スクリプト再生中の仮想 0.25 秒ごとの実時間比・アイドル割合 |
| `$R ihist [--pairs] <snap>` | 実行した ARM 命令の種類（--pairs は連続 2 命令の組）の分布 |
| `$R genrate <image>` | MMU の変換世代・コードページの印付けの頻度・デコード済みページ数 |
| `$R goldencmp <a.jsonl> <b.jsonl>` | 一致確認の結果の比較（レジスタ単位で差を表示） |
| `$R snapdump <snap> [snap2]` | スナップショットのチャンクの一覧・比較 |

## 構成

| 場所 | 役割 |
|---|---|
| `rust/core` | エミュレータのコア（`cerulean-core`。std のみ・プラットフォーム非依存） |
| `rust/core/src/arm` | ARMv4T インタプリタ（デコードキャッシュ・特化・アイドル検出・逆アセンブラ） |
| `rust/core/src/mmu.rs` | MMU / CP15（テーブルウォーク・権限・FCSE・ソフト TLB） |
| `rust/core/src/bus.rs` | 物理アドレス空間（RAM と MMIO の振り分け・監視） |
| `rust/core/src/s3c2410` | S3C2410 周辺機器（UART・割り込み・タイマー・LCD・RTC・ADC/タッチ・SPI など） |
| `rust/core/src/smdk2410` | ボード構成・実行ループ・入力 API・スナップショットの並び |
| `rust/core/src/{snapshot,script,emu,loader}.rs` | 保存形式・入力スクリプト・イベントの適用・イメージローダー |
| `rust/cli` | ネイティブの開発用 CLI（`cerulean`） |
| `rust/web` | ブラウザ版（wasm）のエントリ（段階2・3 で作る） |
| `tools/` | 一致確認・計測・確認のスクリプト |
| `testdata/golden/` | 一致確認の基準（シナリオ・期待値・合成プログラム） |
| `docs/` | Rust 移行の計画 |
