# OS イメージの用意

**日本語** | [English](images.en.md)

CErulean が読み込むのは **Microsoft Device Emulator 用の `.bin`** です。
配布パッケージの `.msi` から取り出してください。実機用 ROM は対象外です。

## 利用条件

- Microsoft が配布したパッケージを使い、パッケージ内のライセンスを確認してください。
- WM5 SDK・WM6 Images の規約は、公開コピーの配布や逆コンパイル等を制限しています。
  WM6 は商用ホスティングも禁止しています。
- **OS イメージ・MSI/CAB・抽出した DLL/EXE・OS を含むスナップショットは公開しないでください。**

## 1. ダウンロード

まず試す場合は、Windows Mobile 5.0 の日本語 VGA 版（480×640）を用意します。
以下のイメージをすべて取得する必要はありません。

### WM5 日本語 VGA 版

使用するパッケージは `Windows Mobile 5.0 Emulator Images for Pocket PC - JPN.msi` です。
Microsoft の配布 URL は現在利用できないため、元の配布ファイルを保存した
Wayback Machine のアーカイブから取得します。

```sh
curl --fail --location --output wm5_emu_jpn.msi \
  'https://web.archive.org/web/20150617130421id_/http://download.microsoft.com/download/b/7/5/b7566ed3-6940-4541-8cf2-3e0fc1fafbc4/Windows%20Mobile%205.0%20Emulator%20Images%20for%20Pocket%20PC%20-%20JPN.msi'
```

### WM5 英語版・WM6 日本語版

- [WM5 SDK for Pocket PC（英語版）](https://www.microsoft.com/en-us/download/details.aspx?id=42)
- [WM6 Localized Emulator Images](https://www.microsoft.com/en-us/download/details.aspx?id=7974)
  → **Windows Mobile 6 Professional Images (JPN).msi**（日本語版）を選択。

```sh
curl --fail --location --output wm5sdk.msi \
  'https://download.microsoft.com/download/d/2/e/d2e43e33-53b0-45b6-ba70-fe6fdc4aa5bb/Windows%20Mobile%205.0%20Pocket%20PC%20SDK.msi'

curl --fail --location --output wm6pro_jpn.msi \
  'https://download.microsoft.com/download/0/1/2/012BFBBA-9FE5-4E68-86C9-D434446D97DD/0411/Windows%20Mobile%206%20Professional%20Images%20(JPN).msi'
```

## 2. 抽出

Python 3 を用意し、[抽出ツール](../tools/images/extract-msi.py)を開いて、
GitHub の「Raw」から内容を `extract-msi.py` という名前で保存します。
ソースの ZIP を取得した場合は、`tools/images/extract-msi.py` をコピーしても構いません。

MSI と抽出ツールを同じ作業フォルダに置き、以下のコマンドをそのフォルダで実行してください。
WM5 の規約は MSI のライセンス表示、WM6 は下記で抽出する `F_License.rtf` で確認できます。

```sh
# WM5 日本語 VGA 版
python3 extract-msi.py wm5_emu_jpn.msi wm5 \
  _2PPC_JP_1_BIN
cp wm5/_2PPC_JP_1_BIN wm5/PPC_JPN_VGA.bin

# WM5 英語版
python3 extract-msi.py wm5sdk.msi wm5 \
  _208PPC_USA_bin
cp wm5/_208PPC_USA_bin wm5/PPC_USA.bin

# WM6 日本語版（QVGA・VGA）とライセンス
python3 extract-msi.py wm6pro_jpn.msi wm6 \
  CGen_PocketPC_0411_PPC_JPN.BIN CGen_PPC_JPN_GSM_VGA_VR.BIN F_License.rtf
cp wm6/CGen_PocketPC_0411_PPC_JPN.BIN wm6/PPC_JPN.bin
cp wm6/CGen_PPC_JPN_GSM_VGA_VR.BIN wm6/PPC_JPN_GSM_VGA.bin
```

抽出先の既存ファイルは上書きしません。ツールは上記パッケージの無圧縮 / MSZIP CAB に対応しています。
Windows では公式インストーラーから取り出す方法もありますが、古い SDK のインストール要件に注意してください。

## 3. 起動

[README](../README.md#セルフホストする)に従ってブラウザ版を開き、使いたい `.bin` を選びます。

| イメージ | 画面サイズ | 構成 |
|---|---|---|
| `wm5/PPC_JPN_VGA.bin` | 480×640 | WM5 日本語版・VGA |
| `wm5/PPC_USA.bin` | 240×320 | WM5 英語版 |
| `wm6/PPC_JPN.bin` | 240×320 | WM6 日本語版・Classic |
| `wm6/PPC_JPN_GSM_VGA.bin` | 480×640 | WM6 日本語版・Professional（電話 UI あり、通話は未対応） |

画面サイズはファイル名から自動判定します。VGA 版は名前の `VGA` を残してください。
判定が合わない場合は起動画面で変更できます。

### 起動にかかる時間

Today 画面が出るまでの時間は、イメージの言語・画面サイズ・版によって大きく異なります。
CLI で測った、Today が出るまでのゲストの時間（仮想時間）の目安です。

| イメージ | Today が出るまで（目安） |
|---|---|
| WM5 英語 QVGA、WM6 英語 Classic・Phone VGA | 約 30 秒以内 |
| WM5 日本語 VGA | 約 60〜70 秒 |
| WM5 英語 VGA | 約 120〜130 秒 |

ブラウザ版は起動直後の約 27 秒分（3.6×10⁹ 命令）を早送りします。残りは通常の速度（等速）で進むため、
WM5 日本語 VGA で約 40 秒、WM5 英語 VGA で約 100 秒、待つことがあります。
待っている間、ゲストは計算ではなく何らかの待ち状態にあるように見えますが、原因はまだ特定できていません。
メニューの速度を「最高速」にすると短くなります。

**言語・イメージによる起動時間の差は、今後改善する予定です。** 一度 Today まで起動すれば自動保存されるので、2 回目以降は「続きから再開」で素早く始められます。
