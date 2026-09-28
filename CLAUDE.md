# CErulean 設計方針

Windows Mobile 5.0（WinCE 5.0）LLE エミュレータ。Go 製。最終的に gomobile bind で iOS/Android に載せる。

## 絶対条件

- **コアは純 Go**。cgo 禁止。UI・OS 依存コードをコア（cmd 以外の全パッケージ）に入れない。
- **CPU はインタプリタ**。iOS では JIT 不可のため。ただし命令デコードと実行を分離してあり、
  将来デコードキャッシュ・ブロック単位実行を追加できる構造を保つこと。
- **既存エミュレータのコードをコピー・参照しない**。実装の根拠は一次資料のみ:
  - ARM Architecture Reference Manual（DDI 0100、ARMv4T/v5TE）
  - Samsung S3C2410 データシート
  - Windows CE 5.0 のドキュメント（BIN 形式、OEMAddressTable 等）
- **仕様が不確かな箇所は推測で埋めない**。`TODO:` コメントで疑問点を残し、ユーザーに質問する。
- 設計判断には短い理由コメントを残す（ユーザーは Go は読めるがエミュレータ開発は初めて）。

## パッケージ境界

```
cmd/cerulean → machine/smdk2410 → { cpu/arm, mmu, bus, device/s3c2410, loader }
                                     cpu/arm → cpu (interface), mmu → bus
```

- `cpu` パッケージは **interface のみ**。コア実装（`cpu/arm`）は将来 Rust 等に差し替えられるよう、
  この境界を跨ぐ依存を作らない。
- **S3C2410 固有の知識（アドレスマップ、レジスタ配置）は `device/s3c2410` と `machine/smdk2410` だけが持つ**。
  cpu・bus・mmu に SoC 固有の定数を入れない。machine は差し替え可能（将来 PXA27x 構成を追加する）。
- ロード時の CE 仮想アドレス→物理アドレス変換（OEMAddressTable 相当）は machine の責務。

## メモリアクセスの設計

- `cpu.Memory` interface（error 返し）を経由する。error はアボート相当で、
  当面はエミュレーション停止、将来はデータアボート例外に変換する。
- リトルエンディアン固定（WinCE/ARM は LE）。
- 性能面: mmu のソフト TLB（4KB 直接マップ）が RAM ページの実体を持ち、
  ヒット時は bus を経由せず直接読み書きする。MMIO は常に bus 経由。
  bus.AddWatch（-watch）が有効な間は fast path を使わない。
  デコードキャッシュは未着手（decode.go/exec の分離は維持すること）。

## スナップショット・入力の設計（2026-09 ユーザー確認済み）

- 全状態は `snapshot` パッケージの形式で保存する（`CERUSNAP`＋形式版数＋flate。
  中身はコンポーネント単位のチャンクで、各チャンクが独立した版数を持つ。末尾の
  バイト数トレーラで書き手と読み手の食い違いを検出）。未知・非対応は必ずエラー。
- **状態を持つフィールドを足したら SaveState/LoadState を更新し StateVersion を上げる**。
  各パッケージの `CheckFields` テストが、保存対象か配線かの分類漏れを検出する。
  MMIO デバイスは machine がバスから列挙し、Stateful でないものはエラーになる
  （状態のない openBus だけ例外）。ソフト TLB も保存する（通し実行との完全一致のため）。
- 入力 API（TouchDown/Move/Up・KeyDown/Up）は machine に置き、スクリプトの解釈は
  純 Go の `script` パッケージ、ファイル読み込みと時刻照合は cmd。時刻は命令数
  （machine.Steps、InstructionsPerSecond=135.2M/仮想秒）で、決定論的。
- 対話フロントエンドは当面作らない。作るならブラウザ型（cmd がローカル HTTP で
  画面配信・入力受付。cgo・GUI ライブラリ不要）を想定しておく。

## 例外・エラーの扱い

- 未実装命令は `arm.UndefinedError{PC, Word, ...}` を返して停止し、CLI が PC と命令語を表示する。
  黙って NOP にしない（デバッグ不能になるため）。
- 未マップアドレスへのアクセスは `bus.BusError`（アドレス付き）。

## テスト方針

- テーブル駆動。フラグ計算（NZCV、シフタキャリー）を重点的に。
- ローダーのテストは合成バイナリで書く（実イメージをリポジトリに入れない）。

## 実イメージについて確認済みの事実（2026-09 検証）

- WM5 Pocket PC SDK（archive.org の `windows-mobile-5.0-pocket-pc-sdk_202305`）内の
  `PPC_USA.bin`（21MB、`tmp/images/` に抽出済み）は **B000FF（BIN）形式**。
- イメージ範囲: start=0x80070000 length=0x01421ED0、エントリ=0x80076CF0、レコード 99 個
  （チェックサム全数一致）。
- **BIN の終端レコードは `{addr=0, len=エントリポイント, checksum=0}`**（addr が 0 で終端）。
- VA 0x80000000 → PA 0x30000000 の変換で実イメージがブートする。
- **カーネルデバッグシリアルは UART1**（0x50004000）。ブートバナー
  「Windows CE Kernel for ARM (Thumb Enabled) Built on Apr 14 2005」が出る。
- **ブートコードは MMU 有効化イディオムに依存**: `mcr p15,c1`（M=1）の直後の
  命令は物理アドレスのままフェッチされる前提（パイプライン残り）。エミュレータは
  「c1 の M 変化後、連続する最大 2 命令は旧変換状態でフェッチ」で近似（mmu パッケージ）。
- 制御レジスタに 0xC000327F を書く（M/A/C/W/R/**V=high vectors**、S=0/R=1）。
  DACR=1（ドメイン0 クライアント）なので **AP 権限チェックが実際に効く**。FCSE(c13) も使う。
- **SDRAM は 128MB 必要**: OEMGetExtensionDRAM が PA 0x34000000〜を署名書き込みで
  プローブする。64MB+折り返しにするとエイリアスを実 RAM と誤検出して二重使用になる。
  後半 32MB は RAMFMD（RAM ディスク）になる。バンク7(0x38000000)と
  バンク0〜5(ROM/フラッシュ)はオープンバス（読み 0）で、NOR フラッシュプローブ
  （AM29LV800）とメモリプローブが正しく失敗する。
- カーネルは FPU 検出のため MRC p10 を、トラップ用に未定義空間
  （bits[27:25]=011,bit4=1）の命令を意図的に実行する。どちらも未定義例外の配送が必要。
- **OEMAddressTable**（VA 0x80076BAC）: VA 0x80000000→PA 0x30000000(64MB)、
  0x84000000→0x10000000、0x86000000→0x18000000、0x88000000→0x20000000、
  0x8A000000→0x28000000、0x8C000000→0x08000000（各 32MB）、
  0x90800000〜0x91A00000→PA 0x48000000〜0x5A000000（周辺機器、各 1MB）、
  0x92000000→0x00000000(32MB)、0x94000000→0x34000000(192MB)。
- **ROM モジュールの特定方法**: イメージ先頭+0x40 の 'CECE' の次が ROMHDR の
  VA（0x8148C10C、392 モジュール・404 ファイル）。TOC の e32_rom から各
  モジュールの vbase/vsize が分かり、PC からモジュール名を引ける。
  ただし XIP モジュールのコード節は圧縮フラグ（0x2000）付きなので、中身は
  イメージから直接読まずに実行時トレース（-trace-from）で見る。
- **PA 0x500F0000 台は Device Emulator 固有の準仮想デバイス**（S3C2410 にない）。
  +0x2080+n*0x20 は dmatrans.dll（DMA トランスポート、4 チャネル）、
  +0x5000 は emulserv.dll（割り込みは GPF3=EINT3・High レベル）。どちらも
  初期化の読み書きだけでブートは止めない（詳細は machine/smdk2410 のコメント）。
  DE 固有モジュールは他に DeviceEmulator_lcd.dll・vcefsd.dll（フォルダ共有）・
  serdma.dll・dmacnect.exe・EmulatorStub.exe がある。
- **LCD**: OAL が起動直後（約 7.8 万命令目）に設定する。LCDCON1=0x6F9
  （TFT・16bpp・ENVID=1）、LCDCON2=0x014FC081（320 行）、LCDCON3=0x0030EF02
  （240 列）、LCDCON5=0xB01（FRM565・HWSWP=1）、**FB は PA 0x33F00000**、
  PAGEWIDTH=240。DeviceEmulator_lcd.dll は RAM の FB に直接描く
  （LCD レジスタは 0x2C に 1 回書くだけ）。HWSWP=1 で下位ハーフワードが左ピクセル
  という解釈で正しい色が出ることを画像で確認済み。
- **クロック**: ブートが MPLLCON=0x000A1031・CLKDIVN=3 を書く → Fin=12MHz で
  FCLK=202.8MHz、PCLK=50.7MHz。Timer4 は TCFG0=0・TCFG1=0（1/2 分周）・
  TCNTB4=25375 で約 1ms 周期。
- **RTC**: OAL は BCDSEC→YEAR→MON→DATE→DAY→HOUR→MIN→SEC の順に読むだけ
  （書き込み・RTCCON 操作なし）。値保持スタブ（全 0）だと日付が 2508 年になり、
  Today 画面の予定・タスク欄が出ない。
- **ブート到達点（2026-09 時点）: Today 画面まで起動する**（-fb-out の PNG で確認）。
  約 22.5 億命令目でスプラッシュ（Windows Mobile ロゴ）、約 34〜35 億命令目で
  Today 画面が段階的に描かれて完成し（35 億命令まで約 2.5 分）、以後はアイドル
  ループ（0x800AFxxx）。マイルストーン2 で「停滞」と見ていたのは起動完了後の
  アイドル（入力待ち）だった。
  エミュレーション速度は約 22M 命令/秒（ソフト TLB 導入後）。
- **タッチパネルは S3C2410 標準の ADC/タッチスクリーン I/F**（touch.dll、VA 0x01530000〜）。
  初期化で GPGCON|=0xFF000000・ADCDLY=50000・ADCCON=0x7200・ADCTSC=0xD3。
  ペンダウンの INT_TC で、自動変換（ADCTSC=0xDC）を 4 回して平均し、Timer3 を
  自動リロードで起動して約 10ms ごとにサンプリング。サンプリング後は
  **ADCTSC=0x1D3（bit8=S3C2440 の UD_SEN 相当）でペンアップを INT_TC 検出**する
  （タイマー経路は UPDOWN を読まないので、アップの割り込みが無いと押しっぱなしになる）。
- **タッチ座標変換はドライバ内の固定式**（キャリブレーションデータなし。レジストリの
  HARDWARE\DEVICEMAP\TOUCH は MaxCalError=7 のみ）。1/4 ピクセル単位で
  X4=(ADCDAT1−85)×960/880、Y4=(1023−ADCDAT0−105)×1280/875（軸入れ替え・Y 反転）。
- **キーは SPI1 のキーボード用マイコン**（kbdmouse.dll、VA 0x014D0000〜）。EINT1
  （GPF1 立ち下がり）1 回ごとに GPB6=Low→SPTDAT1=0xFF→GPB6=High→SPRDAT1 で 1 バイト。
  bit7=1 が離した、bit6〜0 がスキャンコード、直前と同じバイトは無視。スキャンコード→VK
  はドライバ内の固定表（0x00〜0x6F。Enter=0x5A、↑0x6C ↓0x6A ←0x6D →0x6F、App1〜5=0x64〜0x68）。
  **ソフトキー（VK_F1/F2）は表に無い**。初期化時に 0xFF×10 と 3 バイトコマンド
  （1B A0 7B / 1B A1 7A）を送るが応答は読まない。
- 電源ボタン pwrbtn2410.dll は GPF0（EINT0）を設定する（未実装）。
- **マイルストーン4 の到達点**: Today（36 億命令）のスナップショットから、タップで
  Start メニュー → Calendar / Settings が起動し、方向キー・Enter・文字入力も効く。

## 未確定事項・次の課題（随時更新）

- **入力**: ソフトキー（VK_F1/F2）を物理キーとして押す経路が未発見（kbdmouse の表に
  無い。DE 本体のボタンが別経路か要調査）。電源ボタン（EINT0）・UART 受信は未実装。
  キーボード用マイコンの 3 バイトコマンドの意味・データ無し時の応答値（0 としている）・
  チップセレクト（GPB6）の扱いは未確認。
- ADC: ADCTSC bit8（UD_SEN）を S3C2440 と同じ意味で実装しているが、S3C2410 の
  データシートでは予約のはず（DE の独自実装と判断）。割り込み待ちに入った時点で
  既に検出対象の状態なら INT_TC を出す（レベル扱い）のは判断。変換時間
  （(PRSCVL+1)×5 PCLK）・ADCDLY の意味・リセット値は要照合。
- SPI: レジスタ配置・INT_SPI0/1 のビット番号（22/29）は記憶ベースで要照合。
- 0x500F0000 の DE 準仮想デバイスのレジスタの意味（ホスト連携が必要になったら）。
- LCD: LINECNT/VSTATUS は常に 0 を返す（走査は未エミュレート）。垂直同期待ちの
  ポーリングが現れたら仮想時間から生成する。STN モード・パレット形式は未検証。
  LCD レジスタのビット配置は記憶ベースなのでデータシートと要照合。
- RTC: アラーム・ティック割り込み（TICNT）・RTCRST は値保持のみ。BCDDAY の
  起点曜日（1=日曜としている）は要照合。
- オーディオ系スタブの妥当性: DMA「即完了」モデルは CURR_TC==0 の完了
  ポーリングが現れると破綻する。DMASKTRIG の ON_OFF ビット位置（bit1）も要再照合。
- スタブに入れたリセット値（クロック MPLLCON 等、WTCON、GSTATUS1=0x32410000、
  ARM920T キャッシュタイプレジスタ 0x0D172172）と PLL の式は記憶ベース。
  データシートと要再照合。
- MMU: アライメントフォルト（A ビット）未実装（CPU がアドレスをマスクしてから
  発行するため現状は到達しない）。
- 性能: さらに速くするならデコードキャッシュ（現状デコードは約 8%）、
  タイマー Advance のバッチ化（約 5%）、アイドル時の仮想時間早送り
  （OAL のアイドルはスピンなので要設計）。
- タイマー周期の ±1 カウント（TCNTB か TCNTB+1 か）はデータシートの波形図で要確認。
- INTC の PRIORITY（回転アービトレーション）は固定優先度に簡略化中。
- BLX 等 ARMv5TE 拡張は未実装（PXA27x 対応時）。
- LDC/STC・CDP は未実装または未定義例外扱い。
- 日本語版イメージ: 「Localized Windows Mobile 5.0 Pocket PC Emulator Images」
  （JPN 版 msi）の入手先が未発見。archive.org には USA 版 SDK のみ確認。
