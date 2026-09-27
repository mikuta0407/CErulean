# CErulean

Windows Mobile 5.0（Windows CE 5.0 ベース）の LLE エミュレータを目指す Go 製プロジェクト。

Microsoft Device Emulator 向けの WM5 エミュレータイメージ（英語版・日本語版）を、
Samsung S3C2410（ARM920T）構成のマシンで起動することを最初のゴールとする。
将来的には PXA27x 系の実機構成の追加と、gomobile bind による iOS/Android 対応を予定。

## 状態

マイルストーン1（開発中）:

- [x] プロジェクト骨格
- [x] イメージローダー（Windows CE BIN 形式 / .nb0 生形式）と `info` コマンド
- [x] ARM インタプリタの骨組み（データ処理・ロード/ストア・分岐・MRS/MSR・SWI）
- [x] バス・RAM・S3C2410 UART0（送信のみ）と `run` コマンド
- [x] 各命令のユニットテスト（テーブル駆動、フラグ計算重点）
- [x] 実イメージ（WM5 SDK の PPC_USA.bin）での検証
      — ブートコード先頭を実行し、未実装デバイス（GPIO）アクセスで停止するところまで確認

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

# 実行（未実装命令に当たると PC と命令語を表示して停止する）
./cerulean run path/to/nk.bin
```

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
| `mmu` | MMU / CP15（現状はパススルーのスタブ） |
| `bus` | 物理アドレス空間（RAM と MMIO ディスパッチ） |
| `device/s3c2410` | S3C2410 周辺機器（UART など） |
| `machine` | SoC＋周辺機器の構成定義。`smdk2410` が最初のターゲット |
