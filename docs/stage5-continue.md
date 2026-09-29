# 段階5（JIT-to-wasm）の続きの開始用プロンプト

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。
（段階5 の最初のプロンプトは `docs/stage5-kickoff.md`。こちらは 5-2 の完了後の続き。）

---

CErulean（WM5 LLE エミュレータ、Rust 製）の段階5（JIT-to-wasm）の続きをしたい。
まず次を最後まで読むこと: CLAUDE.md、docs/stage5-design.md（設計と、実装で変えた点）、
docs/rust-migration-plan.md の §9 段階5（決定・5-1 と 5-2 の結果・未解決）。コードは
rust/core/src/jit（mod.rs・codegen.rs・wasm.rs・selftest.rs）、rust/core/src/arm/mod.rs の
run_loop・run_page_jit、rust/core/src/arm/code.rs の JIT の枠、rust/web/src/lib.rs の
WebJitHost を必要なだけ読んでから始めること。

## 現状（2026-09-29、5-3 の完了時点）
- 5-1・5-2・5-4 の前半（V8 のメモリ）・5-3（ページをまたぐ連結）が完了。
- 速度（Node）: 起動 4 億命令で JIT あり約 171M（JIT なし約 75M）、Today まで（36 億命令）
  約 105M 命令/秒。ブラウザ: Chrome 155（Mac）で boot-1200M が一致して 378M・メモリ約
  222MB、iPad の Safari 26.2 で経過診断の安定区間が約 570M 命令/秒。
- メモリ（`node --expose-gc tools/bench/jit-mem.mjs`）: 途中の最大の RSS は Today まで
  JIT なし 248MB・JIT あり約 354MB。
- 一致: 全 6 基準シナリオが JIT あり（既定）と閾値 1・1 ブロックずつで完全一致。
- **Mac の Safari 27・iPhone（Safari 27.2）では、ページの JIT 全体（素の JS も）が効かない
  状態がたびたび起きる**（JIT-to-wasm を使う前から。Safari を再起動した直後は速いことも
  あった）。原因は未特定で、ユーザーの判断で後回しにした（計画書の段階5「ブラウザでの JIT」）。
- 残りの費用（Today まで、Node のプロファイル）: 生成コードは命令の約 80% を実行して時間は
  約 34%。本体側はインタプリタ約 24%（サイド出口の後・入口でない PC）、MMIO の読み
  約 10%、ブロックへの出入り約 10%、TLB の埋め直し約 4%。サイド出口は 2,850 万回。

## 次のゴールの候補（ユーザーと相談して選ぶ）
1. サイド出口の後のインタプリタを減らす: 出口の命令だけインタプリタで実行したら、次の
   命令から JIT に戻れるようにする（ブロックの途中の入口をどう持つか。関数の大きさ・
   V8 のメモリとの兼ね合い）。サイド出口の理由の内訳（MMIO・TLB ミス・権限・ページ
   またぎの LDM/STM）を先に数える。
2. MMIO をその場で呼ぶ（設計書 §4 の保留事項。呼んだ後に割り込み線・期限・上限を
   確かめる規則が要る。設計の変更なので実装前にユーザーに確認する）。
3. 5-4 後半: Firefox・boot-today・iPhone（Safari の JIT が効く状態で）の計測、
   CODE_LIMIT の決定。
4. 残りの対象外: MRS/MSR・SWP・BSpin・Thumb（計測で意味がある分だけ。対象外の命令は
   全命令の 1〜1.5%）。

## 注意（前のセッションで分かったこと）
- wasm の速度は LLVM の展開の判断に敏感（CLAUDE.md）。実行ループの周りを変えたら
  JIT なしの速度も web-bench.sh と bench.sh で確かめる。
- 差分テスト（rust/web/tests/jit-diff.mjs、check.sh から seed 1・2）は、まれな値の組み
  合わせを見落とすことがあった。生成を変えたら、わざと誤りを入れて検出できるかを
  確かめてから戻す（前のセッションではこれで差分テストの弱さを見つけた）。
- 5-4 前半の前は、長いシナリオを JIT の閾値 1・1 ブロックずつで走らせると V8 が
  メモリを使い果たし、開発機ごと落ちた。今は 12 億命令で約 400MB だが、生成を大きく
  変えたら jit-mem.mjs で先に確かめる。
- `tools/golden/verify.sh` の JIT ありは `CERULEAN_JIT=1 GOLDEN_RUNNER=wasm`（事前の
  web-build は verify.sh が行う）。計測は `tools/bench/web-bench.sh [-j 64,32]`、単発は
  `node tools/bench/wasm-run.mjs rust/web/pkg-node <命令数> [閾値,数]`。
  プロファイルは `node --cpu-prof --cpu-prof-dir=<新しいディレクトリ>`（同じディレクトリを
  消そうとすると権限の確認で止まる）。

## 進め方の約束（CLAUDE.md・計画書が正）
- 大きな設計判断・依存の追加は実装前に確認を取る。仕様が不確かな箇所は推測で
  埋めず TODO を残して質問する。既存エミュレータのコードは参照しない。
- 機能ごとにテストを書く。停止のたびに短く報告する。
- 大きな出力は tmp/ に置く（/tmp は小さい tmpfs）。
- コミットは論理単位で、メッセージの末尾にセッションの帰属表示を付ける。
