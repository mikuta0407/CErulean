# ストレージカード（PC カード＋CompactFlash）の設計

開始用プロンプトは `docs/storage-card-kickoff.md`。この文書は決定事項・観察結果・未決事項を
まとめる（随時更新）。

## 決定事項

- **方式は案 A**（2026-09-29 ユーザー決定）: 82365SL 互換の PC カードコントローラ＋
  CompactFlash（ATA）のカード。中身は FAT のディスクイメージ。
  - 案 B（vcefsd のフォルダ共有）を選ばなかった理由: ブートから Today まで vcefsd.dll は
    一度も実行されず（`--sample 5000` で 0 回）、0x500F0000 台へのアクセスも dmatrans・
    emulserv の既知のものだけだった。読み込みのきっかけ（ホストからの通知）とレジスタの
    規約の 2 段を観察だけで決める必要があり、不確かさが大きい。
- 一次資料は公開されているものを Claude が集める（2026-09-29 ユーザー決定）。
- 以下も 2026-09-29 ユーザー決定:
  - コントローラはボードに常に置く。既定の構成（カードなし）でも基準が変わるので
    `tools/golden/regen.sh` で作り直す。
  - ディスクの中身はスナップショットに入れる（空でない部分だけを保存）。
  - CIS の製造者 ID・文字列は独自の値にする（SanDisk の値をそのまま使わない）。
  - **カードに簡単にデータを入れられること**（いつでもオンデマンドでなくてよい）。
    「抜く → イメージを編集 → 挿す」でよい: CLI はホストのフォルダから FAT イメージを
    作る・取り出す、ブラウザはカードを抜いている間にファイルを出し入れする。

## 一次資料（`tmp/docs/`、非コミット。テキスト化は `tmp/pdf2txt.sh`）

| ファイル | 内容 | 入手先 |
|---|---|---|
| `pd6710db.pdf` | Cirrus Logic CL-PD6710/'22 Preliminary Data Sheet v3.1（1997-05）。82365SL 互換のレジスタ | pcmcia-cs.sourceforge.net/specs/pd6710db.pdf |
| `cfspc3-0.pdf` | CF+ and CompactFlash Specification Rev 3.0（CFA、2004）。属性メモリ・構成レジスタ・ATA のコマンド | rumkin.com/reference/aquapad/cfspc3-0.pdf |
| `sandisk-cf.pdf` | SanDisk CompactFlash Memory Card OEM Product Manual v1.0（2009）。CIS の全バイトと意味（Table 6-1）、IDENTIFY の内容 | farnell.com/datasheets/39782.pdf |
| `fatgen103.pdf` | Microsoft FAT32 File System Specification 1.03 | osdever.net/documents/fatgen103.pdf |

- Intel 82365SL そのもののデータシートは bitsavers に無く、商用のデータシートサイトにしか
  見当たらなかった。PD6710 は 82365SL 互換のレジスタ（Register Compatibility Type: 365）を
  持つので、これを基準にする（Cirrus 拡張のレジスタも PD6710 の記述どおりにする）。
- PCMCIA の Metaformat（タプルの書式）は非公開。CF の仕様書もそちらを参照するだけなので、
  タプルは SanDisk の Table 6-1（実在のカードの CIS の全バイトと各フィールドの意味）を根拠にする。

## 観察結果（2026-09-29、PPC_USA.bin）

調べ方: ブートを `--watch 0x10000000-0x2FFFFFFF --watch 0x500F0000-0x500FFFFF` で Today まで
（約 7 分）。探索用にコントローラ（ID だけ応答）を仮に置いたビルドでも観察した
（`tmp/sc/explore-pcic.patch`）。

### プローブ（コントローラなし＝今の構成）

- 約 4.3959 億命令目に `pcc_smdk2410.dll`（VA 0x01740000〜、ceddk.dll の
  WRITE/READ_PORT_UCHAR 経由）が PA 0x110003E0 に 8 ビットで 0 を書き、0x110003E1 を読む。
  0（オープンバス）なので以後何もしない。
- 判定は `sub r3,r0,#0x82; cmp r3,#2`（0x01742E40）: **Chip Revision が 0x82〜0x84 なら
  コントローラあり**。PD6710 の Chip Revision は 0x82（Interface ID=10: メモリと I/O、
  Revision=0010）。

### プローブの直前のボード設定（pcc_smdk2410 が行う）

- BWSCON の bank2 のビット（11:8）= 0xD（16 ビット・WAIT 有効・UB/LB）、BANKCON2 = 0x7FFF。
- GPFCON: GPF3 = EINT3。GPFUP: GPF3 のプルアップ有効。EXTINT0 の EINT3 = 010（立ち下がり）。
- GPGCON: GPG0 = EINT8。GPGUP: GPG0 のプルアップ無効。EXTINT1 の EINT8 = 001（High レベル）。
- 解釈: EINT3 がコントローラの -INTR（管理割り込み＝カードの状態変化。負論理なので
  立ち下がり）、EINT8 がカードの IRQ（ISA の IRQ は正論理なので High レベル）。実装して、
  挿入の割り込みでドライバが 04h を読み、ATA の割り込みが通ることを確かめた。
- emulserv.dll（約 4.365 億命令目）が High レベルに設定するのは **EINT11**（EXTINT1=0x1110）で、
  EINT3 ではない（以前 CLAUDE.md に「EINT3」とあったのは誤り。2026-09-29 に GPIO の監視で確認）。

### バンク2 の配置（コントローラの ID を返した場合）

- I/O 空間: **PA 0x11000000 + ISA の I/O ポート**（インデックス 0x3E0・データ 0x3E1）。
- メモリ空間: **PA 0x10000000 + ISA のメモリアドレス**。メモリ窓 0 を System 0x000000〜
  0x7FFFFF、Card Offset の REG ビット（0x15 の bit6）=1 で属性メモリに向け、
  `pcc_serv.dll`（0x01556154）が PA 0x10000000 から偶数番地を 1 バイトずつ読む（CIS）。

### ドライバの手順（カードありに見せた場合）

1. 初期化: 03h=00、16h=16（Misc Control 1）、1Eh=12（Misc Control 2）、17h=80（FIFO）、
   3Ah〜3Fh（タイミング）、05h=0C（カード検出・Ready の管理割り込みを有効）、
   03h=10（管理割り込みを -INTR へ）、01h を読む。
2. 約 4.424 億命令目: 04h（Card Status Change）・01h を読み、05h=0F、06h（Mapping Enable）の
   bit5 を立てる。02h（Power Control）を読む。
3. 約 4.846 億命令目: 02h=93（Card Enable・VCC・VPP1=11）。約 5.04 億命令目: 03h=50
   （リセット解除・I/O カード）。
4. メモリ窓 0 を属性メモリに設定して 06h の bit0 を立て、CIS を読む（全部 0 なので
   約 0x3E6 まで読んで諦める）。

### カードを実装した後（2026-09-29）

- 検出の表（レジストリ `Drivers\PCMCIA\Detect`）: 10 serial.dll DetectModem・50 atadisk.dll
  DetectATADisk・60 NE2000・99 SRAMDisk。atadisk は CISTPL_FUNCE（ディスク・PC Card-ATA）で
  自分のカードと判断する。
- atadisk.dll は解析済みの構成の表（CISTPL_CFTABLE_ENTRY）から **I/O の範囲が 2 個の構成**
  （プライマリ・セカンダリの AT の固定ディスク）を VCC 5.0V → 3.3V の順に探し、無ければ
  メモリの構成を試す（メモリの構成は失敗した）。SanDisk と同じ構成 2・3 を CIS に入れて通った。
- atadisk の設定: I/O 窓 0x81F0〜0x81F7・0x83F6〜0x83F7（カードは下位 10 ビットで 1F0h 等と
  見る）、COR=0x42（構成 2・レベル割り込み）、**Card IRQ Select = 3**（CIS の推奨は 14）。
  IRQ3 が EINT8 に配線されていると判断した。
- ブートで挿していても、Today で挿しても（ホットプラグ）File Explorer に「Storage Card」が
  出て、長いファイル名・フォルダが見える。ゲストが作ったフォルダをホストで読め、
  `fsck.fat` で誤りなし。抜くと File Explorer から消える。

## 実装（2026-09-29）

- 置き場所: コントローラ（PD6710）とカード（CF）は SoC に依らない部品なので
  `rust/core/src/pccard/`（`pd6710.rs`・`cf.rs`）。外部割り込み（EINTPEND・EINTMASK・EXTINT の
  エッジ／レベル）は `s3c2410/eint.rs`（ボードの部品が駆動するピンだけを見る）。バンク2 のアドレスの割り当てと
  EINT への配線は `smdk2410` が持つ（S3C2410 固有の知識を他に入れない規則）。
- コントローラはボードに常に居る（実機の SMDK2410 と同じく、カードの有無と独立）。
  このため既定の構成でもドライバの初期化が進み、ゲストから見える動作が変わる
  → 基準は `tools/golden/regen.sh` で作り直す（理由をコミットに残す）。
- カード: CIS（属性メモリの偶数番地）・構成レジスタ（Configuration Option Register 等）・
  ATA のタスクファイル（メモリ／I/O のどちらの構成でも）・セクタの読み書き。
  最初はコマンドを即座に完了させる（BSY を立てない）。完了の割り込みはカードの IRQ。
- カードの挿入・抜去は入力イベント（スクリプトのコマンド）として記録する。
- スナップショット: machine の版数 2。pcic・cf・cf:blk（空でない 64KB の区画）のチャンク。
  版数 1 は「コントローラは初期状態・カードなし」として読む。
- 基準: 4.4 億命令目（プローブの直後）から変わる（1M 命令ごとのハッシュで、それより前は
  変更前と一致することを確認）。`tools/golden/regen.sh` で作り直した。JIT（`1,1` も）で一致。
- 外からの出し入れ: `rust/fat`（cerulean-fat。FAT12/16/32 の読み書き・LFN・FAT16 の作成。std のみ）。
  CLI は `cerulean card new|ls|put|get|rm|mkdir` と `run --card F --card-out F`、スクリプトの
  `card insert <file>`・`card eject [file]`。ブラウザ版はメニューの「ストレージカード」
  （OPFS の cards/card.img。抜いている間だけ編集。挿抜は記録され、記録の書き出しで挿したカードの
  イメージも出す）。確認は `tools/browser/app-card.mjs`（記録を CLI で再生してハッシュが一致）。

## 残り・未確認（コードの TODO）

- IRQ3 以外の IRQ ピンの配線、バンク2 の上の 2 つの範囲の外の配線。
- 先頭 64KB のメモリ窓（DS は置けないとするがドライバは使う）。存在しないソケット B の読み値。
- ATA のコマンドは即座に完了する（BSY が見えない）。パルスモードの割り込みはレベルとして出す。
- ブラウザ版: フォルダのドラッグ＆ドロップ（ファイル選択の「フォルダを入れる」は可）。
- NE2000（docs/network-kickoff.md）は同じコントローラに Card トレイトの実装を足す。
