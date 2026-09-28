# CErulean

Windows Mobile 5.0（Windows CE 5.0 ベース）の LLE エミュレータを目指す Go 製プロジェクト。

Microsoft Device Emulator 向けの WM5 エミュレータイメージ（英語版・日本語版）を、
Samsung S3C2410（ARM920T）構成のマシンで起動することを最初のゴールとする。
将来的には PXA27x 系の実機構成の追加と、gomobile bind による iOS/Android 対応を予定。

## 状態

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

Rust 移行（進行中、2026-09〜）: コアを Rust に移し、最初はブラウザ（wasm）で
動かす。計画は [docs/rust-migration-plan.md](docs/rust-migration-plan.md)。

- [x] 段階0（準備）: Go 版に一致確認の道具（`-result`・`-checkpoint`・`-trace-hash`）、
      基準シナリオと期待値（`testdata/golden/`）、Rust のワークスペースの骨組み（`rust/`）
- [ ] 段階1: コアの移植（インタプリタ。ネイティブで Go 版と完全一致）
- [ ] 段階2〜5: wasm での計測、ブラウザ版、インタプリタの高速化、JIT-to-wasm

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
cd ../.. && go run ./cmd/cerulean info tmp/images/PPC_USA.bin
```

## ビルドと実行

```sh
go build ./cmd/cerulean

# イメージの情報表示（開始アドレス・長さ・エントリポイント）
./cerulean info path/to/nk.bin

# 実行（カーネルのデバッグシリアル出力が標準出力に流れる。
# 未実装のデバイス等に当たると PC と直前の命令履歴を表示して停止する）
./cerulean run path/to/nk.bin

# トレース実行（ディスアセンブル付き）・ステップ数制限
./cerulean run -trace -max-steps 1000000 path/to/nk.bin

# Today 画面が出るところまで実行し、停止時の画面を PNG に保存
./cerulean run -max-steps 3600000000 -fb-out screen.png path/to/nk.bin

# 起動の様子を 1 億命令ごとの連番 PNG で見る（RTC を固定して再現可能に）
./cerulean run -rtc 2006-01-02T15:04:05 -max-steps 3600000000 \
  -fb-out shot.png -fb-every 100000000 path/to/nk.bin

# 物理アドレス範囲へのアクセスを PC 付きで記録（周辺機器の調査用）
./cerulean run -watch 0x4D000000-0x4D000FFF -max-steps 100000000 path/to/nk.bin
```

### 入力スクリプトとスナップショット

```sh
# 一度だけ: Today 画面まで起動してスナップショットを保存（約 50 秒）
./cerulean run -rtc 2006-01-02T15:04:05 -snap-save today.snap@3600000000i \
  -max-steps 3600000001 path/to/nk.bin

# 以後はスナップショットから数秒で再開し、スクリプトで操作する
./cerulean run -snap-load today.snap -script calendar.txt path/to/nk.bin
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
`press 名前 [押下時間]`・`shot ファイル`・`snap ファイル`・`quit`。書式の詳細は
`script` パッケージのコメントを参照。

`run` の全フラグは `./cerulean` を引数なしで実行すると表示される。

### ブラウザから操作する（serve）

```sh
./cerulean serve -snap-load today.snap -dir rec/
# → http://127.0.0.1:8080/ をブラウザで開く
```

- 画面のクリック・ドラッグがタッチ。画面をクリックしてフォーカスがある間は、
  PC のキーがハードウェアキーになる（矢印・Enter・英数字・Space/BS/Tab/Esc/Del・
  修飾キー、F1〜F5 = App1〜App5）。ソフトキーは画面下端のタップで操作する
- 等速・2 倍・最高速の切り替え、一時停止、スナップショット保存（`-dir` に保存）
- 「記録開始」で起点のスナップショットを保存し、「記録停止」で入力を命令数つきの
  スクリプトと停止時の画面（`-expected.png`）に書き出す。
  `./cerulean run -snap-load rec-….snap -script rec-….script` で再生すると、
  最後に同じ画面が `-replay.png` に書かれる
- 認証が無いので、既定ではローカル（127.0.0.1）でだけ待ち受ける

## 開発

```sh
go test ./...
go vet ./...

# コミット前の確認（Go と Rust の全テスト、wasm のビルド）。
# 必要なツール: rustup・wasm-bindgen-cli（rust/Cargo.toml と同じ版）・Node.js
tools/check.sh
```

設計方針は [CLAUDE.md](CLAUDE.md) を参照。

### 一致確認（Go 版と Rust 版）

`testdata/golden/` に基準シナリオ（リセット起点。イメージ＋固定の RTC＋絶対命令数の
スクリプト）と、Go 版で作った期待値（CPU 状態・RAM・UART1・画面のハッシュ）がある。
定義は [testdata/golden/README.md](testdata/golden/README.md)。

```sh
# 期待値との照合（実イメージのシナリオは CERULEAN_IMAGE が必要。無ければ合成だけ）
CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh go
# 期待値の作り直し
CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/regen.sh [シナリオ名...]

# run の一致確認用フラグ
./cerulean run -rtc 2006-01-02T15:04:05 -max-steps 1200000000 \
  -result r.jsonl -checkpoint 400000000 \
  -trace-hash 10000000 -trace-hash-ram 100000000 -trace-hash-out th.txt tmp/images/PPC_USA.bin
```

### 調査・計測の道具（`tools/`）

| 道具 | 用途 |
|---|---|
| `tools/bench/bench.sh` | 基準のリビジョン（既定 HEAD）と作業ツリーの速度を交互に計測 |
| `go run ./tools/segspeed <snap> <script>` | スクリプト再生中の仮想 0.25 秒ごとの実時間比・アイドル割合 |
| `go run ./tools/ihist <snap>` | 実行した ARM 命令の種類の分布 |
| `go run ./tools/genrate <image>` | MMU の変換世代・コードページの印付けの頻度 |
| `go run ./tools/goldencmp <a.jsonl> <b.jsonl>` | 一致確認の結果の比較（レジスタ単位で差を表示） |

## パッケージ構成

| パッケージ | 役割 |
|---|---|
| `cmd/cerulean` | CLI フロントエンド（PC 開発用）と、ブラウザ型フロントエンド `serve` |
| `emu` | フロントエンド共通の実行制御（入力イベントを命令境界で適用・記録。純 Go） |
| `loader` | イメージローダー（B000FF / .nb0 / 合成プログラムの命令語テキスト .words） |
| `cpu` | CPU コアの境界 interface（実装非依存） |
| `cpu/arm` | ARMv4T インタプリタ実装 |
| `mmu` | MMU / CP15（ARMv4 テーブルウォーク・権限チェック・FCSE） |
| `bus` | 物理アドレス空間（RAM と MMIO ディスパッチ） |
| `device/s3c2410` | S3C2410 周辺機器（UART・割り込み・タイマー・LCD・RTC・ADC/タッチ・SPI など） |
| `machine` | SoC＋周辺機器の構成定義。`smdk2410` が最初のターゲット（入力 API もここ） |
| `snapshot` | 全状態の保存形式（SoC 非依存のコンテナとエンコーダ） |
| `script` | 入力スクリプトの解釈（純 Go。CLI 以外からも使える） |
| `rust/` | Rust 版（移行中）: `core`（cerulean-core）・`cli`・`web` |
| `tools/` | 一致確認・調査・計測の道具（上記） |
| `testdata/golden/` | 一致確認の基準（シナリオ・期待値・合成プログラム） |
