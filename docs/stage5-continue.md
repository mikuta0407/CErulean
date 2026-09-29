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

## 現状（2026-09-29、コミット ab1b5a6 の時点）
- 5-1（最小の JIT）・5-2（ページ単位の関数とブロックの連結・対象の拡大・関数テーブルでの
  呼び出し）が完了。生成コードで全命令の約 96% を実行する。
- 速度（Node、起動 4 億命令）: JIT なし約 77M、JIT あり約 155M 命令/秒（実時間 135.2M の
  約 1.15 倍）。boot-today 37 秒（JIT なし約 50 秒）、taps-5 の通し 37.6 秒（同 54 秒）。
  JIT なしの速度はネイティブ・wasm とも JIT を足す前と同じ。
- 一致: 全 6 基準シナリオが JIT あり（既定の閾値 64・32 ブロック）で完全一致。閾値 1・
  1 ブロックずつ（`CERULEAN_JIT=1,1`）は boot-1200M と合成 2 本だけ確認済み。
- **未解決の最大の問題: V8 のメモリ**。JIT ありで RSS が約 260MB 増える（起動 12 億命令で
  530MB 対 267MB。生成した wasm は 7MB）。`node --liftoff-only` では増えず、既定
  （Liftoff → TurboFan）で増えるので、大きなページ関数の最適化コンパイルの作業領域と
  見ている（GC でも減らない。小さなモジュールを 800 個作っても増えないことは確認済み）。
  メモリアクセスごとに TLB の確認（十数命令）を展開しているのが関数を大きくしている。
  閾値 1・1 ブロックずつでは boot-1200M で 2GB に達する。
- ブラウザ（Chrome/Firefox/Safari、iPhone）では JIT をまだ一度も動かしていない。
  計測ページ（rust/web/www/bench）には JIT の切り替えがまだない。

## 今回のゴール（順に。各段階で一致確認と計測をしてコミットする）
1. **メモリを減らす（5-4 の前半）**。案: TLB の確認を関数内の補助関数（同じモジュールの
   wasm 関数）に出す、ページ関数の大きさ（命令数・バイト数）に上限を設けて分ける、
   br_table の表を小さくする（関数内のブロックの先頭だけの密な表にする）。
   `node --expose-gc` で RSS を測る道具を tools/ に置き（今は scratchpad の使い捨て）、
   変更前後で比べる。速度が落ちないことも tools/bench/web-bench.sh で確かめる。
2. **ブラウザで動かす**: 計測ページに JIT の有無・閾値の切り替えを足し、Chrome/Firefox/
   Safari（Mac）と iPhone（実機を直接操作。iPhone ミラーリング経由だと JIT が効かない）で
   一致と速度・メモリを見てもらう（tools/serve-bench.py で配信。ユーザーに依頼する）。
   iOS Safari のメモリ上限に注意（計画書 §6.2）。コード量の上限 CODE_LIMIT（今は 32MB の
   仮置き。jit/mod.rs の TODO）もここで決める。
3. **ページをまたぐ連結（5-3）**: 戻りの理由の内訳（jitStats の exit_page・exit_other）では、
   ページを出る戻りが多い。関数テーブルを生成コードに import して、別のページの関数へ
   call_indirect で移る案（世代・code_cur_va・コードページの無効化との整合を設計してから）。
   設計が大きく変わるなら実装前にユーザーに確認する。
4. 残りの対象外: MRS/MSR・SWP・BSpin（アイドルスキップの印）・Thumb は今は対象外。
   計測で意味がある分だけ。

## 注意（前のセッションで分かったこと）
- wasm の速度は LLVM の展開の判断に敏感（CLAUDE.md）。実行ループの周りを変えたら
  JIT なしの速度も web-bench.sh と bench.sh で確かめる。
- 差分テスト（rust/web/tests/jit-diff.mjs、check.sh から seed 1・2）は、まれな値の組み
  合わせを見落とすことがあった。生成を変えたら、わざと誤りを入れて検出できるかを
  確かめてから戻す（前のセッションではこれで差分テストの弱さを見つけた）。
- 長いシナリオを JIT の閾値 1・1 ブロックずつで走らせると V8 がメモリを使い果たし、
  開発機ごと落ちることがある（前のセッションが落ちた）。メモリを減らすまでは避ける。
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
