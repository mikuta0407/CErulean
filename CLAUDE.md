# CErulean 設計方針

Windows Mobile 5.0（WinCE 5.0）LLE エミュレータ。Go 製。最終的に gomobile bind で iOS/Android に載せる。

## 絶対条件

- **コアは純 Go**。cgo 禁止。UI・OS 依存コードをコア（cmd 以外の全パッケージ）に入れない。
- **CPU はインタプリタ**。iOS では JIT 不可のため。ただし命令デコードと実行を分離してあり、
  将来デコードキャッシュ・ブロック単位実行を追加できる構造を保つこと。
- **既存エミュレータのコードをコピー・参照しない**。実装の根拠は一次資料のみ:
  - ARM Architecture Reference Manual（DDI 0100、ARMv4T/v5TE）
  - Samsung S3C2410 データシート（S3C2410X User's Manual Rev 1.1。bitsavers のミラー
    bitsavers.trailing-edge.com/components/samsung/S3C204x/S3C2410/ から入手し
    `tmp/docs/`（非コミット）に置く。テキスト化は pypdf で `tmp/docs/um.txt`）
  - Windows CE 5.0 のドキュメント（BIN 形式、OEMAddressTable 等）
- **仕様が不確かな箇所は推測で埋めない**。`TODO:` コメントで疑問点を残し、ユーザーに質問する。
- 設計判断には短い理由コメントを残す（ユーザーは Go は読めるがエミュレータ開発は初めて）。

## パッケージ境界

```
cmd/cerulean → emu → machine (interface) ← machine/smdk2410 → { cpu/arm, mmu, bus, device/s3c2410, loader }
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
- 命令実行の高速化（2026-09 ユーザー確認済み。いずれも全状態の完全一致が条件）:
  - デコードキャッシュ（cpu/arm/codecache.go）: 物理 4KB ページ単位のデコード済み
    命令。無効化は書き込み保護方式（デコードしたページを指す TLB エントリの
    wram を外し、そのページへのストアだけ遅い経路で CPU に通知。mmu/code.go）。
    実行中ページの有効性は MMU の世代で管理し、世代が上がると SetGenHook で
    CPU の記憶を捨てる。世代は TLB 全無効化・特権状態/FCSE PID の変化・コード
    ページの印付け・CPU が覚えている（watched）エントリの詰め替えで上がる。
  - ブロック実行: machine が「次のデバイスイベントの期限を生む命令まで」の
    命令数を Core.Run に渡す。ブロック途中の MMIO では Executed() から仮想時間を
    追いつかせ（catchUp）、期限が早まれば LimitRun で打ち切る。
  - 頻出命令の特化（special.go）: デコード時にフィールド取り出し済みの専用関数を
    選ぶ。汎用版との一致はランダム差分テスト（special_test.go）で確認する。
  - LDM/STM は 1 ページ内・TLB ヒットなら RAMRun で直接読み書き。
  - Core に配線フィールドを足したら Reset の作り直しで引き継ぐこと
    （TestResetKeepsWiring。runs を落として高速化が無効になっていたことがある）。
  - 効果がなく取り下げたもの（計測済み）: CPU 側のデータ用ページキャッシュ
    （TLB 引きは減るがキャッシュミス待ちが移るだけ）、PGO（かえって約 5% 遅い）、
    GOAMD64=v3（誤差）。計測は tmp/m5ref/bench.sh（HEAD と作業ツリーを交互に
    3 回。この環境は ±4% 程度ばらつく）。
- 実行ループは machine の RunUntil（2026-09 ユーザー確認済み）。以下はどれも
  「1 命令ずつ Step した場合と全状態が完全一致する」ことが条件:
  - デバイス時間のまとめ進め: PCLK ティックを溜め、次のデバイスイベント
    （NextEvent）の期限か、時間を持つデバイスの MMIO アクセス（timedDev）・
    入力 API・スナップショット保存の直前にだけ Advance する。時間を持つデバイスを
    足したら NextEvent を実装し、timedDev で包み、syncTime に加えること。
  - アイドルスキップ: cpu/arm の PollLoop が副作用のない 3 命令ループを検出し、
    machine が 2 周の状態一致を確かめて、次のイベント直前まで命令数と仮想時間
    だけ進める（-trace・-watch 中は無効。命令履歴はループの PC 列で補う）。
    ロード先は RAM か、bus.StableReader を実装したデバイスのレジスタ（読んでも
    副作用がなく、値がデバイスイベント・書き込み・外部入力でしか変わらないもの。
    現状 ADC）。ソフト TLB は直接マップなので、コードとロード先が同じスロットに
    当たるループは毎周詰め替えが起きてスキップされない（正しい挙動）。
  - 検証は変更前バイナリとの出力比較（UART・レジスタ・履歴・PNG・スナップ
    ショット）。tmp/m5ref に比較スクリプトがある（非コミット）。

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
- 対話フロントエンドはブラウザ型の `cerulean serve`（2026-09 ユーザー確認済み）。
  画面は /frame のロングポーリング（RGBA）、入力は POST /input（JSON）。
  エミュレーション goroutine がマシンを専有し、壁時計との同期は serve 側
  （コアは決定論的なまま）。記録は emu.Session.Inject の命令数つき入力を
  script.Format で絶対命令数（@<n>i）として書き出す。

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
- **OAL のアイドルは割り込み待ちのスピン**: 0x800AFDE4〜 の
  `LDR r3,[r4]`（r4=0x814C8708）/ `CMP r3,#0` / `BEQ` の 3 命令で、割り込み
  ハンドラが RAM の変数を書くまで回る（直前に 0x800AFDDC で変数を 0 にし、
  0x800779D8 の関数で IRQ を許可する）。Today から Calendar を操作する間でも
  実行命令の約 85% がこのループ（2026-09 の -sample 観察）、ブート中は約 9%。
- **touch.dll は ADC の変換完了を空回りで待つ**: 0x015317F4〜 の `LDR r3,[r4]`
  （r4 = ADCCON）/ `TST r3,#0x8000`（ECFLG）/ `BEQ`。ADCDLY=50000 なので
  1 回の待ちが長く、操作中の実処理の約 1 割を占めていた（2026-09 観察）。
  StableReader でアイドルスキップの対象にした。
- **操作中の実処理の内訳**（2026-09、タップ 5 回の記録を再生して PC を集計）:
  nk.exe 41%・DeviceEmulator_lcd.dll 16%・coredll 14%・touch.dll 11%（上記の待ち）・
  aygshell/gwes/imaging 等。カーネル分は PC が散らばっており待ちループではない。
  1 タップで約 1 億命令の本当の処理があり、実時間で追うには 135M 命令/秒が要る。
- **性能（2026-09、マイルストーン5）**: 実処理の区間は約 75〜80M 命令/秒
  （実時間の約 0.55 倍。マイルストーン4 終了時は約 21M）。Today 完成まで
  36 億命令が約 49 秒。タップ 5 回の記録（13.4 仮想秒）の再生は 2.0 秒で、
  タップ直後の区間は実時間の 0.9〜2 倍、アプリ起動の本処理は約 0.5 倍。
  残りの律速は命令ごとの間接呼び出しと、ゲストメモリ・デコード表への
  アクセスのキャッシュミス（Run のループに現れる）。
- **マイルストーン4 の到達点**: Today（36 億命令）のスナップショットから、タップで
  Start メニュー → Calendar / Settings が起動し、方向キー・Enter・文字入力も効く。

## 今後の計画（2026-09 時点。後回しと判断したもの）

- **ソフトキーの物理キー入力**: kbdmouse の表に VK_F1/F2 が無い。画面のソフトキー
  表示のタップで同じ操作ができるので後回し。着手するなら、DE 本体ボタンの経路を
  観察する（他の入力系モジュール: conshid/kbdhid・emulserv・0x500F0000 の準仮想
  デバイスへのアクセスを -watch とトレースで確認）。
- **電源ボタン**（pwrbtn2410.dll、GPF0/EINT0）: 押すとサスペンド（OEMPowerOff・
  スリープ・起床要因）の実装が必要になり範囲が大きい。UI 操作には不要なので後回し。
- 性能改善の続き。アプリ起動のような実処理の区間はまだ実時間の約 0.5 倍。
  Go のインタプリタとしては頭打ちに近い（上記の計測）。さらに上げるなら
  ネイティブ化（下記）かブロック単位のより大きな変換（スーパー命令等）。
- ネイティブ言語（Rust/C/C++）化の検討（2026-09 ユーザーと相談）: 同じ設計なら
  1.5〜3 倍程度の見込み（推定・未計測）。ただし cgo が必要（現方針は禁止）で、
  Go との境界呼び出しが 1 回数十 ns かかるため、境界は cpu.CPU ではなく
  「CPU＋MMU（ソフト TLB）＋RAM」をまとめた外側（RunUntil 単位と MMIO だけが
  往復する形）にする必要がある。Go でデコードキャッシュ等を入れて計測してから判断する。

## 未確定事項・次の課題（随時更新）

- データシート照合（2026-09 実施）で確認済み: INT_SPI0/1=22/29、SPI のレジスタ配置・
  リセット値、ADC のレジスタ・リセット値・変換時間、LCD のビット配置と 16bpp の
  HWSWP の画素順、クロック（LOCKTIME/MPLLCON/UPLLCON/CLKCON/CLKSLOW）・WTCON・
  GSTATUS1 のリセット値、PLL の式、DMASKTRIG の ON_OFF=bit1、RTC のオフセット。
  食い違いを直したもの: タイマー周期は TCNTB+1 カウント（Figure 10-2）、ADC の
  変換前に ADCDLY（PCLK）の遅延が X/Y それぞれに入る（Figure 16-3）。

- **入力**: ソフトキー（VK_F1/F2）を物理キーとして押す経路が未発見（kbdmouse の表に
  無い。DE 本体のボタンが別経路か要調査）。電源ボタン（EINT0）・UART 受信は未実装。
  キーボード用マイコンの 3 バイトコマンドの意味・データ無し時の応答値（0 としている）・
  チップセレクト（GPB6）の扱いは未確認。
- ADC: ADCTSC bit8 はマニュアルでは「予約（0 にすること）」。touch.dll がこれを
  ペンアップ検出（S3C2440 の UD_SEN 相当）に使うので、DE 独自の実装と判断して
  合わせている。割り込み待ちに入った時点で既に検出対象の状態なら INT_TC を出す
  （レベル扱い）のも判断。プリスケーラ無効時の ADC クロックと、割り込み待ち中に
  ADCDLY 間隔で INT_TC を繰り返す動作は未実装。
- 0x500F0000 の DE 準仮想デバイスのレジスタの意味（ホスト連携が必要になったら）。
- LCD: LINECNT/VSTATUS は常に 0 を返す（走査は未エミュレート）。垂直同期待ちの
  ポーリングが現れたら仮想時間から生成する。STN モード・パレット形式は未検証。
- RTC: アラーム・ティック割り込み（TICNT）・RTCRST は値保持のみ。BCDDAY は
  1〜7 だが起点曜日はマニュアルに記載なし（1=日曜としている）。
- オーディオ系スタブの妥当性: DMA「即完了」モデルは CURR_TC==0 の完了
  ポーリングが現れると破綻する。
- ARM920T のキャッシュタイプレジスタ 0x0D172172 は記憶ベース（S3C2410 の
  マニュアルには無い。ARM920T TRM で要照合）。
- MMU: アライメントフォルト（A ビット）未実装（CPU がアドレスをマスクしてから
  発行するため現状は到達しない）。
- 性能: アイドルスキップは ARM の 3 命令ループのみ対応（Thumb や別形のループが
  現れたら PollLoop に追加）。Thumb はデコードキャッシュ・特化の対象外
  （WM5 の操作中の実行では 0%）。
- INTC の PRIORITY（回転アービトレーション）は固定優先度に簡略化中。
- BLX 等 ARMv5TE 拡張は未実装（PXA27x 対応時）。
- LDC/STC・CDP は未実装または未定義例外扱い。
- 日本語版イメージ: 「Localized Windows Mobile 5.0 Pocket PC Emulator Images」
  （JPN 版 msi）の入手先が未発見。archive.org には USA 版 SDK のみ確認。
