# Preparing OS images

[日本語](images.md) | **English**

CErulean loads **`.bin` files for the Microsoft Device Emulator**.
Extract them from the distribution `.msi` packages. Real-device ROMs are not supported.

## Terms of use

- Use packages distributed by Microsoft, and read the license included in each package.
- The terms of the WM5 SDK and WM6 Images restrict redistributing public copies, decompiling and similar acts.
  WM6 also prohibits commercial hosting.
- **Do not publish OS images, MSI/CAB files, extracted DLLs/EXEs, or snapshots that contain the OS.**

## 1. Download

To try it first, prepare the Windows Mobile 5.0 Japanese VGA edition (480×640).
You do not need to obtain all the images below.

### WM5 Japanese VGA edition

The package to use is `Windows Mobile 5.0 Emulator Images for Pocket PC - JPN.msi`.
Microsoft's download URL is no longer available, so fetch it from a
Wayback Machine archive of the original file.

```sh
curl --fail --location --output wm5_emu_jpn.msi \
  'https://web.archive.org/web/20150617130421id_/http://download.microsoft.com/download/b/7/5/b7566ed3-6940-4541-8cf2-3e0fc1fafbc4/Windows%20Mobile%205.0%20Emulator%20Images%20for%20Pocket%20PC%20-%20JPN.msi'
```

### WM5 English edition and WM6 Japanese edition

- [WM5 SDK for Pocket PC (English)](https://www.microsoft.com/en-us/download/details.aspx?id=42)
- [WM6 Localized Emulator Images](https://www.microsoft.com/en-us/download/details.aspx?id=7974)
  → Select **Windows Mobile 6 Professional Images (JPN).msi** (Japanese edition).

```sh
curl --fail --location --output wm5sdk.msi \
  'https://download.microsoft.com/download/d/2/e/d2e43e33-53b0-45b6-ba70-fe6fdc4aa5bb/Windows%20Mobile%205.0%20Pocket%20PC%20SDK.msi'

curl --fail --location --output wm6pro_jpn.msi \
  'https://download.microsoft.com/download/0/1/2/012BFBBA-9FE5-4E68-86C9-D434446D97DD/0411/Windows%20Mobile%206%20Professional%20Images%20(JPN).msi'
```

## 2. Extract

Install Python 3, open the [extraction tool](../tools/images/extract-msi.py),
and save its contents from GitHub's "Raw" view as `extract-msi.py`.
If you downloaded the source ZIP, you can instead copy `tools/images/extract-msi.py`.

Put the MSI and the extraction tool in the same working folder and run the following commands in that folder.
The WM5 terms are shown in the MSI's license screen; for WM6, check `F_License.rtf`, which is extracted below.

```sh
# WM5 Japanese VGA edition
python3 extract-msi.py wm5_emu_jpn.msi wm5 \
  _2PPC_JP_1_BIN
cp wm5/_2PPC_JP_1_BIN wm5/PPC_JPN_VGA.bin

# WM5 English edition
python3 extract-msi.py wm5sdk.msi wm5 \
  _208PPC_USA_bin
cp wm5/_208PPC_USA_bin wm5/PPC_USA.bin

# WM6 Japanese edition (QVGA and VGA) and the license
python3 extract-msi.py wm6pro_jpn.msi wm6 \
  CGen_PocketPC_0411_PPC_JPN.BIN CGen_PPC_JPN_GSM_VGA_VR.BIN F_License.rtf
cp wm6/CGen_PocketPC_0411_PPC_JPN.BIN wm6/PPC_JPN.bin
cp wm6/CGen_PPC_JPN_GSM_VGA_VR.BIN wm6/PPC_JPN_GSM_VGA.bin
```

Existing files in the output folder are not overwritten. The tool supports the uncompressed / MSZIP CABs of the packages above.
On Windows you can also extract them with the official installers, but beware of the install requirements of the old SDKs.

## 3. Launch

Open the browser app following the [README](../README.en.md#self-hosting) and select the `.bin` you want to use.

| Image | Screen size | Configuration |
|---|---|---|
| `wm5/PPC_JPN_VGA.bin` | 480×640 | WM5 Japanese, VGA |
| `wm5/PPC_USA.bin` | 240×320 | WM5 English |
| `wm6/PPC_JPN.bin` | 240×320 | WM6 Japanese, Classic |
| `wm6/PPC_JPN_GSM_VGA.bin` | 480×640 | WM6 Japanese, Professional (with phone UI; calls are not supported) |

The screen size is detected automatically from the file name. For VGA editions, keep `VGA` in the name.
If the detection is wrong, you can change it on the start screen.

### Boot time

The time until the Today screen appears varies greatly with the image's language, screen size and edition.
These are rough figures, measured with the CLI, for the guest time (virtual time) until Today appears.

| Image | Until Today (approx.) |
|---|---|
| WM5 English QVGA, WM6 English Classic / Phone VGA | within about 30 seconds |
| WM5 Japanese VGA | about 60–70 seconds |
| WM5 English VGA | about 120–130 seconds |

The browser app fast-forwards the first ~27 seconds (3.6×10⁹ instructions) after startup. The rest runs at normal speed,
so you may wait about 40 seconds for WM5 Japanese VGA and about 100 seconds for WM5 English VGA.
During that wait the guest appears to be in some kind of wait state rather than computing, but the cause has not been identified yet.
Setting the speed to "Maximum" in the menu shortens the wait.

**We plan to improve the differences in boot time between languages and images.** Once Today has been reached the state is saved automatically, so from the second time on you can start quickly with "Resume".
