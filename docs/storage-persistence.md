# 本体ストレージの永続化の調査（2026-09-30）

目的: イメージから起動し直しても本体（My Device）の中身が残るようにできるか。ROM は
書き換えない（ハードを用意するだけ）方針で調べた（ユーザー了承）。結論は **どちらの経路も
無改変のイメージでは成り立たない**。永続化はスナップショット（Device Emulator の Saved State
と同じ）で行う。

## NOR フラッシュ（amdnord.dll）

- 約 5800 万命令目に `amdnord.dll`（VA 0x03DF0000〜）が PA 0 に AMD の自動選択の列
  （F0 → 5555h に AA → 2AAAh に 55 → 5555h に 90。いずれもワードアドレス、16 ビット）を送り、
  0h・2h を読む。今はオープンバスなので「AM29LV800_Init: Bad manufacturer or device code」で
  諦める。
- レジストリの `MSFLASH for AMD Nor` のプロファイル（FATFS・MountAsBootable・MountAsRoot・
  FormatTfat・fsreplxfilt）で使われる。つまり成功すれば本体のファイルシステムの根になる。
- 一次資料 AMD Am29LV800B データシート（Publication 21490 Rev G。octopart のミラー）の
  コマンド・自動選択の値（製造者 01h・ボトムブート 225Bh）で Am29LV800BB を仮に置いて観察した
  （実装は `tmp/net/nor-experiment.patch`・`tmp/net/nor.rs.bak`。非コミット）:
  - ドライバは製造者 01h・装置 225Bh だけを受け付ける（0x03DF684C の比較）。
  - その後、ブロック数を **0x1FE（510）× 64KB と決め打ち**し（0x03DF6704）、ブロック 1〜3 を
    ボトムブートの小さいセクタ（8K・8K・32K）として扱う。1MB のチップの構成（19 セクタ）と合わない。
  - 各ブロックの末尾（+0xFFE0〜）を読んで全部を消去する（510 回）。消去の番地は
    `(n − 3) << 16` を 16MB で折り返した値になり、状態の読み出しは 32MB の範囲に及ぶ。
- 実在のチップ（1MB）では 1MB ごとに同じ中身が見えて壊れ、32MB の架空のフラッシュを作るのは
  「推測で埋めない」方針に反する。**載せない**（今どおりオープンバス）。

## RAMFMD（RAM ディスク）を残す

- `OEMGetExtensionDRAM` が PA 0x34000000〜（VA 0x94000000）の 32MB を RAMFMD に予約する
  （PA 0x36000000〜 の 32MB はシステムに返す側）。
  RAMFMD は先頭に署名 "FLSH"（0x48534C46）と構成（0x1F81 セクタ・0x1000 バイト等）を置き、
  署名が合わなければ「RAMFMD: Clearing RAM storage region」で消去する（`ramfmd.dll`
  0x01665448〜）。
- OAL の OEMInit は SDRAM の起動引数（PA 0x30020000）の +0x4E（u16）の bit0 を「ソフトリセットの
  印」として見る（0x800AFC44〜）。立っていなければ「soft reset flag not set, forcing clean boot」と
  出して立てる。この印を立てて起動するとクリーンブートの表示は消える。
- しかし **OEMGetExtensionDRAM がどの起動でも PA 0x34000000 に 55555555h・AAAAAAAAh を書いて
  メモリの有無を確かめる**（0x800AFA68。印に関わらない）ので、RAMFMD の署名が必ず壊れ、
  毎回消去される。領域の中身と印を戻して起動し直す方法（`run --ram-preload` で試した）は
  成り立たない。

## 調べるための道具（残したもの）

- `cerulean run --ram-preload PA:FILE`（リセットの前に RAM に置く）・`--ram-dump PA:N:FILE`
  （止めたときに書き出す）。

## ついでに見つけた食い違い（TODO）

- GSTATUS2（0x560000B4）のリセット値はデータシートでは 0x1（PWRST）だが、スタブは 0 を返す。
  ブートは bit1（OFFRST）だけを見るので今の動作には影響しないが、直すと基準が変わる。
