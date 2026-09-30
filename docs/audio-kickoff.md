# 音を出す の開始用プロンプト

**結果（2026-09-30、実装済み）**: 原因は DMA のスタブ（ON_OFF を書くと即完了の割り込みを
1 回だけ出す）だった。ドライバは自動リロードで 2 枚のバッファを交互に使い、割り込みごとに次の
DISRC を書くので、2 回目以降の割り込みが来ずに起動音の再生のまま止まっていた。DMA（チャネル 2・
I2SSDO）と IIS の送信 FIFO を仮想時間で動かすように実装し（s3c2410 の dma.rs・iis.rs）、
CLI の `--audio-out`・ブラウザ版のメニューの「音を出す」で鳴る。設計と確認済みの事実は
CLAUDE.md の「音」とコードの先頭のコメント。以下は開始時のメモ（調査前の仮説を含む）。

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。

---

CErulean（WM5 LLE エミュレータ、Rust 製）で **ゲストの音をブラウザで鳴らしたい**。
設計方針・Rust の規則・確認済みの事実は CLAUDE.md、計画は docs/rust-migration-plan.md、
ブラウザ版の現状は README と rust/web/www/app を読んでから始めること。

## 分かっていること（2026-09-29 の調査）

- ROM に `s3c2410x_wavedev.dll`（S3C2410 標準の IIS＋DMA の音声ドライバ。VA 0x014F0000〜、
  大きさ 0xA000）・`waveapi.dll`・`sndplay.exe`・`xmeevtsnd.dll` と、MenuPop.wav・Default.wav・
  notify.wav 等の音がある。
- ブート中に wavedev が IIS（PA 0x55000000〜）と DMA（PA 0x4B000000〜。どのチャネルかは
  要確認。前回はチャネル 2 の帯 0x4B000080〜 への書き込みを見た）を初期化するのは見えた
  （各レジスタに数回の読み書き）。
- **ところが再生が起きない**: Settings → Sounds & Notifications で「Screen taps」を有効にして
  タップしても、Start メニュー・キー操作をしても、IIS にも DMA（全チャネル）にもアクセスが
  1 回も無い（UART1 にも何も出ない）。原因は未調査。疑わしいもの:
  - 今の DMA は「即完了」の簡略化（CLAUDE.md の「オーディオ系スタブの妥当性」）。初期化の
    途中で DMA の完了割り込みや CURR_TC を待って失敗し、ドライバが無効になっている
  - 音声コーデック（SMDK2410 は Philips UDA1341TS。L3 バスは GPIO で叩く）の扱い
  - ドライバの IST が割り込み（INT_DMA2 等）を待ったまま
  - そもそも音の経路が DE 固有の準仮想デバイス（0x500F0000 台）である可能性
- 既定では「Screen taps」の音は無効。Events・Programs・Notifications は有効。
  Sounds & Notifications は Start (12,8) → Settings (60,191) → (46,180)、「Screen taps」の
  チェックは (15,129)。

## 進め方の案

1. 原因の調査: wavedev の初期化（ブート中）と、音を鳴らす操作の後の実行を追う。
   `--trace --trace-from N` と `--watch`（IIS・DMA・INTC・GPIO・0x500F0000 台）。wavedev の
   PC 範囲（0x014F0000〜0x014FA000）に入った命令だけを抜き出して流れを見る。UART1 の
   デバッグ出力（ドライバの RETAILMSG）も見る。PlaySound が確実に起きる操作（例: Sounds &
   Notifications の Notifications タブの再生ボタン、アラーム）も探す。
2. 一次資料で IIS・DMA の動作を詰める（S3C2410 のデータシート `tmp/docs/um.txt` の IIS・DMA の
   章。UDA1341TS のデータシートは入手先をユーザーに聞く）。DMA は転送の進み（CURR_TC・
   CURR_SRC）と完了割り込みを仮想時間に合わせて出す（1 サンプルの時間はクロック設定
   = IISPSR・IISMOD から決まる）。**時間を持つデバイスを足すなら next_event・board.rs の同期
   （sync_time・update_deadline・MMIO の振り分け）に加える**（CLAUDE.md の実行の設計）。
3. サンプルの取り出し: コアは IIS の FIFO に入ったサンプル（と、その命令数）を溜め、フロント
   エンドが取り出す（UART1 と同じ形）。コアは壁時計を見ない。
4. ブラウザ: Worker から取り出したサンプルを AudioWorklet に送って鳴らす。エミュレーションが
   実時間より遅い・速い区間の扱い（バッファの伸び縮み・無音の補い・早送り中は鳴らさない）を
   決める。最初のユーザー操作まで AudioContext が始められない制約に注意。メニューに音の
   オン・オフと音量。
5. CLI: 音を WAV に書き出すオプション（一致確認にも使える: サンプル列の SHA-256）。

## 約束（CLAUDE.md・計画書が正）

- 既存エミュレータのコードは参照しない。仕様が不確かな箇所は推測で埋めず TODO を残して聞く。
- DMA・IIS の動作を変えるとゲストから見える動作が変わる。基準を `tools/golden/regen.sh` で
  作り直すなら、理由（何を直したか）をコミットに残す。状態を足したらスナップショットの
  版数を上げる（旧版の読み込みを残す）。
- 決定論: サンプルの内容と命令数は、どの実行方式（インタプリタ・JIT）でも同じになること。
- 依存の追加はユーザーの承認を取る。
- コミット前に `tools/check.sh`、`CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh`、
  ブラウザ版は `node tools/browser/app-e2e.mjs tmp/images/PPC_USA.bin tmp/app-e2e`。

## 調べ方のメモ

- PC からモジュール名を引く方法、Today のスナップショットの作り方、スクリプトでの操作は
  docs/storage-card-kickoff.md の「調べ方のメモ」と同じ。
- `--watch` は 1 命令ずつ進むので遅い（ブート全体で数分）。Today のスナップショットから
  短く走らせる。
