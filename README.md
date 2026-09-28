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

Today 画面の完成まで約 35 億命令（現状の速度で約 2.5 分）かかる。入力（タッチ・キー）は
次のマイルストーン。

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

`run` の全フラグは `./cerulean` を引数なしで実行すると表示される。

## 開発

```sh
go test ./...
go vet ./...
```

設計方針は [CLAUDE.md](CLAUDE.md) を参照。

## パッケージ構成

| パッケージ | 役割 |
|---|---|
| `cmd/cerulean` | CLI フロントエンド（PC 開発用） |
| `loader` | イメージローダー（B000FF / .nb0） |
| `cpu` | CPU コアの境界 interface（実装非依存） |
| `cpu/arm` | ARMv4T インタプリタ実装 |
| `mmu` | MMU / CP15（ARMv4 テーブルウォーク・権限チェック・FCSE） |
| `bus` | 物理アドレス空間（RAM と MMIO ディスパッチ） |
| `device/s3c2410` | S3C2410 周辺機器（UART・割り込み・タイマーなど） |
| `machine` | SoC＋周辺機器の構成定義。`smdk2410` が最初のターゲット |
