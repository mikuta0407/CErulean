# ストレージカード（外からデータを渡す）の開始用プロンプト

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。

---

CErulean（WM5 LLE エミュレータ、Rust 製）に、**エミュレータの外（ブラウザ・CLI）から中身を
読み書きできるストレージ** を足したい。目的は「任意のデータ（ファイル）をエミュレータの中の
WM5 に渡す・取り出す」こと。WM5 からは Storage Card（FAT のカード）として見えるのが理想。
設計方針・Rust の規則・確認済みの事実は CLAUDE.md、計画は docs/rust-migration-plan.md
（§7 ブラウザ版・§8 依存）、ブラウザ版の現状は README と rust/web/www/app を読んでから始めること。

## 分かっていること（2026-09-29 の調査）

- WM5 の Settings → Memory の Storage は 31.42MB 固定。OAL が拡張 DRAM から RAMFMD 用に
  32MB を決め打ちで確保している（ブートログ「OEMGetExtensionDRAM: reserving 0x02000000 bytes
  … for RAMFMD」）。SDRAM を 256MB にしても Storage は変わらず Program が 37→101MB に増える
  だけだった（ゲストのコードを書き換えない限り Storage 本体は増やせない）。
- ROM にある関係するモジュール（ROMHDR の TOC から。調べ方は後述）: `pcc_smdk2410.dll`
  （PC カードのソケット制御）・`pcmcia.dll`・`atadisk.dll`（CF/ATA のディスク）・`fatfsd.dll`・
  `fsdmgr.dll`・`vcefsd.dll`（Device Emulator のフォルダ共有）・`ne2000.dll`。SD 系のドライバは無い。
- **ブート中に `pcc_smdk2410.dll` が PC カードコントローラを探している**: 約 4.4 億命令目に
  PA 0x110003E0 へ 8 ビットで 0 を書き（インデックス = 識別・版数のレジスタ）、0x110003E1 を
  読んで 0（今はオープンバス）なので諦める。0x3E0/0x3E1 は Intel 82365SL 互換（ExCA）の
  インデックス／データのポートで、SMDK2410 ではバンク2（nGCS2、0x10000000〜）の I/O 空間に
  置かれていると見られる（要確認）。コントローラが見つかれば、カードの挿入 → CIS（カード
  属性情報）の読み出し → ドライバ（CF なら atadisk、NE2000 なら ne2000）の読み込み、と進むはず。
- もう一つの経路は Device Emulator 固有のフォルダ共有（`vcefsd.dll`、PA 0x500F0000 台の
  準仮想デバイス。CLAUDE.md の「0x500F0000」の項）。仕様は公開されていない。
  Device Emulator のソース（shared source 版を含む）は「既存エミュレータのコード」なので
  **参照しない**。調べるならゲストのドライバの動きを観察して決める。

## 進め方の案（最初にユーザーと決める）

1. 方式の選択（提案して決める）:
   - 案 A: 82365SL 互換の PC カードコントローラ＋CompactFlash（ATA、True IDE でなく PC カード
     の I/O モード）のカード。中身は FAT のディスクイメージ（OPFS・ファイル）。外からの読み書きは
     フロントエンドで FAT を解釈するか、ディスクイメージごと入れ替える。PC カードの部分は
     ネットワーク（NE2000 の PC カード、docs/network-kickoff.md）と共有できる。
   - 案 B: vcefsd のフォルダ共有（ホストのフォルダがそのまま見える。外からの操作が一番楽）。
     プロトコルをゲストの観察だけで決められるかを先に見極める。
   - 案 C: その他（例: ゲストが既に使う RAMFMD の中身を外から読み書き。ただし FS の形式は
     WM5 の内部形式で、外からの操作は難しい）。
2. 一次資料を集める（tmp/docs に置く。非コミット）: Intel 82365SL（または Cirrus Logic
   CL-PD6710/6720）のデータシート、PC Card Standard（CIS のタプル）、CF の仕様
   （CompactFlash Association）、ATA/ATAPI の仕様、FAT の仕様（Microsoft の FAT32 File
   System Specification）。入手先が分からなければユーザーに聞く。
3. 観察: `--watch` で 0x10000000〜0x2FFFFFFF・0x500F0000 台へのアクセス、`--trace` で
   pcc_smdk2410 の初期化の手順を見る。コントローラの応答（識別値・ソケットの状態・割り込み線
   ＝どの EINT か）を決める。割り込み線・カード検出の配線が分からなければ TODO にして聞く。
4. 実装（コアは std のみ・決定論的）: カードの挿入・抜去は入力イベントとして記録する
   （スクリプトのコマンドを足す）。ディスクイメージの中身はスナップショットに入れるか、別の
   ファイルとしてイメージ ID と同じように照合するかを決める（ゲストから見える状態なので、
   再現性に関わる）。
5. フロントエンド: ブラウザ版のメニューにカードの挿入・抜去、ファイルの出し入れ（FAT の解釈は
   フロントエンド側か、コアの外のクレート）。CLI にもディスクイメージの指定を足す。

## 約束（CLAUDE.md・計画書が正）

- 既存エミュレータのコードは参照しない。仕様が不確かな箇所は推測で埋めず TODO を残して聞く。
- コアの動作が変わる（新しいデバイスが見える）ので、基準（testdata/golden/expected）への
  影響を確認する。カードを挿さない既定の構成では今の基準と一致させる（コントローラが
  見えるだけで基準が変わるなら、理由をコミットに残して `tools/golden/regen.sh`）。
- 状態を持つデバイスを足したらスナップショットの版数を上げる（旧版の読み込みを残す）。
- 依存の追加（FAT のクレート等）はユーザーの承認を取る。
- コミット前に `tools/check.sh`、コアの動作に関わる変更では
  `CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh`、ブラウザ版は
  `node tools/browser/app-e2e.mjs tmp/images/PPC_USA.bin tmp/app-e2e`。

## 調べ方のメモ

- ROM のモジュール一覧と、PC からモジュール名を引く: イメージ先頭+0x40 の 'CECE' の次が
  ROMHDR の VA。ROMHDR（0x54 バイト）の後に TOC（32 バイト×nummods。+16 がファイル名の VA、
  +20 が e32_rom の VA で、e32_rom の +8 が vbase・+0x14 が vsize）。前回は Python の使い捨ての
  スクリプトで読んだ（必要なら `cerulean` のサブコマンドにする）。
- Today のスナップショットは `cerulean run --rtc 2006-01-02T15:04:05 --snap-save
  tmp/today.snap@3700000000i --max-steps 3700000001 tmp/images/PPC_USA.bin` で作れる。
  操作はスクリプト（`@<命令数>i tap x y`・`+3s shot a.png`）で再現する。Start は (12,8)、
  Start メニューの Settings は (60,191)、Settings の System タブは (74,284)。
