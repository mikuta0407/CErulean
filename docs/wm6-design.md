# Windows Mobile 6（日本語版）への対応

2026-09-30。WM6 の日本語版の Device Emulator 用イメージ（Professional の msi の Classic 構成）を、WM5 と同じ smdk2410
構成で Today 画面まで起動し、タップ・キー・アプリ起動（設定）まで確認した。WM5 の動作は
変えていない（基準シナリオは全部一致）。

## イメージ

- 入手元: Microsoft「Windows Mobile 6 Localized Emulator Images」（Download Center ID 7974）。
  archive.org の `WM6LocalizedEmulatorImages` に全言語の msi がある。
  `Windows Mobile 6 Professional Images (JPN).msi`（228,013,056 バイト、SHA-256
  `7856461d9009d699de8c00278a43ce81797cba0bce60ce8dc55b9dc339fe155d`）。
- msi の CAB の `CGen_PocketPC_0411_PPC_JPN.BIN` が QVGA・電話なしの Pocket PC（同梱のスキン
  `Pocket_PC.xml` の題名は「Windows Mobile 6 Classic」。電話つきの `*_GSM_*` が Professional）。
  ここでは `PPC_JPN.bin` と呼ぶ。100,663,296 バイト（96MB）、SHA-256
  `14270634549027eb83fb2b9ecd5e0fe23935a811f76b402a9ebd8bb5e33a1ea7`。
  他に Phone 版（`*_GSM_*`）・VGA・Square の BIN があるが試していない。
- **B000FF 形式ではなく、バンク0 の NOR フラッシュの中身そのもの**:
  - 先頭 64KB が IPL（「Microsoft Windows CE IPL Version 1.2 for DeviceEmulator」）。先頭の命令は
    `b 0x1000`（リセットベクタ）。+0x40 の 'CECE' の次が IPL の ROMHDR（VA 0x80029B7C。
    IPL は VA 0x80021000 にリンクされ、RAM にコピーして動く）。
  - +0x30000 からカーネルの XIP 領域（+0x30040 の 'CECE' の次が ROMHDR VA 0x880CD4E8。
    physfirst=0x88030000、ulRAMStart=0x80070000、ulRAMEnd=0x83EEF000）。
  - OEMAddressTable（VA 0x88036D14）は WM5 と同じ並びで、0x88000000 → PA 0（96MB）が加わり、
    WM5 の 0x88000000/0x8A000000/0x8C000000 の行がない。
- ローダーの判別（`loader::is_flash`）: B000FF の署名がなく、4KB の倍数で 128MB 以下、
  先頭が条件 AL の B、+0x40 が 'CECE'。`.nb0` の名前なら従来どおり .nb0 として扱う。

## 起動の流れ（観察）

1. リセットで PA 0 から IPL。IPL は AM29LV800 のドライバでフラッシュの ID を確かめ
   （下記）、ブート設定の署名がないので既定値で進み、`Jumping to VA 0xA8030000 PA 0x30000`。
2. カーネル「Built on Mar 4 2007」。OEMGetExtensionDRAM・RAMFMD は WM5 と同じ
   （PA 0x34000000〜の 32MB を RAMFMD）。`Coldinit phase 2 failed! Error code=0x800700B7` は
   WM5 と同じく出る。
3. 約 1.5e9 命令でスプラッシュ、約 3.0e9 命令で日本語の Today（`--rtc 2007-01-02T12:00:00`
   で「火曜日 2007年1月2日」）。以後のアイドルは 0x88070078〜 の 3 命令（フラッシュ上の XIP。
   アイドルスキップが効く）。ネイティブで 30 億命令が約 35 秒。
4. タッチ・キーは WM5 と同じ経路で効く（タップで Start、下キーで項目の移動、「設定」が開く）。

## 周辺の機能の確認（2026-09-30）

- **ストレージカード**（`--card`）: WM6 の日本語版では「メモリ カード」として見え、ファイルの読み
  （`hello.txt`）とフォルダの作成（`--card-out` のイメージに入る。日時はゲストの時刻）ができた。
- **フォルダ共有**（`--share`）: 「Storage Card」として見える。WM6 の vcefsd.dll は一覧の前に
  ボリュームの根「\」を名前で引く（コマンド 11h・番号 -1）。WM5 は引かないので装置が不正な引数
  （57h）で断っており、一覧が空になっていた。根はディレクトリとして答えるように直した
  （deshare.rs）。読み・フォルダの作成ができた。
- **ネットワーク**（`--nic --net`）: NE2000 のドライバで DHCP（10.0.2.15）・DNS が通る。WM5 と同じく
  「設定 → 接続 → ネットワークカード」の接続先を「インターネット設定」にすると、IE で
  http://example.com/ が表示された。HTTPS（`--net-ca`）は試していない。
- **音**（`--audio-out`）: 44.01kHz・ステレオで起動音が出る（Today が出る頃、約 29 億命令目）。
- **タッチ**: メモの手書きで、指定した座標に線が描かれることを確かめた（ずれなし。touch.dll の
  換算は WM5 と同じ式・定数で、キャリブレーションは使われない）。

## 高解像度（VGA・正方形）のイメージ（2026-09-30）

- 画面の大きさはイメージの中にはなく、Device Emulator がスキンの設定（`displayWidth`・
  `displayHeight`。VGA は 480×640、VGA の正方形は 480×480、正方形は 240×240、WM6 の
  `*_GSM_QVGA_VR` は 320×320。msi の CGen_PPC_XML_properties.xml の DisplayWidth/Height）から起動前に
  RAM に置く。WM5 JPN の `PPC_JPN_VGA.bin` の OAL（VA 0x800AF89C〜）は PA 0x30020000（BSP の引数の
  領域）の +0x44 が署名 0xDE12DE34 なら +0x48 幅・+0x4A 高さ・+0x4C 色数を使い（「Using
  emulator-specified video parameters」）、なければ 240×320・16 ビット。置かないと VGA 用の大きな
  部品を 240×320 に描く。WM6 の IPL は引数の領域を初期化し直すが +0x44〜 は残り、同じく効く。
- `Machine::set_display`（CLI `--screen 480x640`、ブラウザ版は起動画面の「画面」。既定の「自動」は
  名前の VGA・Square から決める）で置く。指定しなければ何も置かない（今までと同じ）。
- touch.dll の換算は画面の幅・高さの 4 倍を変数に持つので、タッチの逆変換も LCD に設定された
  大きさから求める（`touch_screen_size`）。VGA のメモの手書きで指定どおりの位置に描けた。
- WM5 JPN の VGA 版と WM6 の電話つき VGA 版（`PPC_JPN_GSM_VGA_VR.BIN`）が 480×640 で Today まで
  起動し、操作できる（ブラウザ版も Chrome で確認）。基準シナリオ `wm6-vga-settings`
  （`CERULEAN_IMAGE_WM6_VGA`）。

## NOR フラッシュ（`rust/core/src/norflash.rs`）

一次資料は Am29LV800B のデータシートのコマンドの表。ゲストの使い方は IPL とカーネルの
フラッシュドライバのトレース。

- IPL は AA→5555h・55→2AAAh・90→5555h（語アドレス。A10〜A0 では 555h/2AAh）で自動選択に
  入り、語 0 = 0x0001（AMD）・語 1 = 0x225B（Am29LV800BB）を確かめ、F0 で戻す。書き込み
  （AA/55/A0 → データ）とトグルビット（DQ6）の待ちのコードもある。
- カーネル側のドライバ（VA 0x03E59F64〜。フラッシュ上で XIP 実行）も同じ手順で ID を読むが、
  **自動選択の間も同じフラッシュから命令とリテラルを読んで動き続ける**。実チップならどの番地も
  ID のデータになり動かないはずなので、Device Emulator のフラッシュは ID を先頭の数語でだけ
  返すと判断した（id_read。先頭の 4KB ページの読みだけを装置に問い、他は中身を直接読む）。
- 書き込みは 1→0 だけ、消去は 0xFF。時間はかけない（瞬時に完了）。
- 2026-09-30 の範囲（Today・設定を開くまで）では、ゲストは書き込み・消去をしていない
  （スナップショットのフラッシュがイメージと一致）。セクタの大きさ（先頭 64KB はブート
  セクタ、以後 64KB）は推定で、ゲストの消去を見たら確かめる。

## エミュレータの構造

- バスに `Kind::Flash`（`Bus::map_flash`）: 中身はアリーナ（RAM と同じ直接の読み・ソフト TLB・
  デコードキャッシュ・JIT が使える）、書き込みは常に装置（コマンド）。装置が読み出し配列で
  ないモードの間は、先頭の 4KB ページだけ直接読みをやめて装置に問う。
- MMU はフラッシュの物理範囲に直接書き込みの位置（wram）を持たせない（`set_read_only`）。
  フラッシュのモードが変わる・装置が中身を書き換えると、バスが `take_remap` で知らせ、MMU は
  書き換えた範囲のデコード結果を捨て、TLB の RAM の位置を引き直す（tag/pa/perm は変えないので
  ゲストから見える TLB の状態は変わらない）。
- マシンは `load_image` がフラッシュのイメージを見たときだけバンク0 にフラッシュを載せる
  （WM5 の構成・メモリの使い方は変わらない）。スナップショットは machine の版数 6 で
  フラッシュの大きさを machine チャンクに、コマンドの状態を `flash`、中身を `flash:data`
  （96MB）に持つ。読み込むとフラッシュの有無に合わせて構成を作り直す。
- 基準シナリオ `wm6-today-settings`（`CERULEAN_IMAGE_WM6`）。ネイティブ・wasm・JIT（1,1 も）・
  アイドルスキップなし・スナップショットからの再開で一致を確認した。

## 未確定・今後

- ID を返す範囲（先頭の 3 語だけか、セクタごとか）、セクタの構成、書き込み・消去の時間。
- CFI・アンロックバイパス・消去の中断は未実装（使われていない）。
- Phone・VGA・Square の BIN、WM6 Standard（Smartphone）、WM6.1/6.5 は未確認。
- ブラウザ版は同じ wasm の経路で読み込める（形式の判別はコア）。メモリは RAM 128MB＋
  フラッシュ 96MB、スナップショットは約 235MB になる。実ブラウザでの確認はまだ。
