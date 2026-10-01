# OS イメージの用意

CErulean が読み込むのは **Microsoft Device Emulator 用の `.bin`** です。
配布パッケージの `.msi` から取り出してください。実機用 ROM は対象外です。

## 利用条件

- Microsoft の公式配布を使い、パッケージ内のライセンスを確認してください。
- WM5 SDK・WM6 Images の規約は、公開コピーの配布や逆コンパイル等を制限しています。
  WM6 は商用ホスティングも禁止しています。
- **OS イメージ・MSI/CAB・抽出した DLL/EXE・OS を含むスナップショットは公開しないでください。**

## 1. ダウンロード

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

Python 3 を用意し、[抽出ツール](../tools/images/extract-msi.py)を
`extract-msi.py` という名前で MSI と同じ作業フォルダに保存します。以下もそのフォルダで実行してください。
WM5 の規約は MSI のライセンス表示、WM6 は下記で抽出する `F_License.rtf` で確認できます。

```sh
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

[README](../README.md#起動する)に従ってブラウザ版を開き、使いたい `.bin` を選びます。

| イメージ | 画面サイズ | 構成 |
|---|---|---|
| `wm5/PPC_USA.bin` | 240×320 | WM5 英語版 |
| `wm6/PPC_JPN.bin` | 240×320 | WM6 日本語版・Classic |
| `wm6/PPC_JPN_GSM_VGA.bin` | 480×640 | WM6 日本語版・Professional（電話 UI あり、通話は未対応） |

画面サイズはファイル名から自動判定します。VGA 版は名前の `VGA` を残してください。
判定が合わない場合は起動画面で変更できます。
