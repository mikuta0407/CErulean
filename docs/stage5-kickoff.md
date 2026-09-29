# 段階5（JIT-to-wasm）の開始用プロンプト

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。

---

CErulean（WM5 LLE エミュレータ、Rust 製）の段階5（JIT-to-wasm）を始めたい。
計画は docs/rust-migration-plan.md（特に §3.2・§6.2・§8・§9 の段階4 と段階5）、
設計方針・Rust の規則・確認済みの事実は CLAUDE.md、現状は README を参照。
まず計画書と CLAUDE.md を最後まで読み、rust/core/src/arm（mod.rs・ir.rs・special.rs・
exec_arm.rs・code.rs・idle.rs）、mmu.rs、smdk2410（mod.rs・run.rs・board.rs）、
rust/web/src/lib.rs、tools/golden を必要なだけ読んでから始めること。

## 現状（2026-09-29 時点）
- 段階0〜2 と段階4（4-1・4-2）が完了。段階3（ブラウザ版の UI）は段階5 の後に回す
  （ユーザー確認済み）。
- 実行方式はインタプリタのみ。デコード済み命令は 8 バイトの IR
  （`arm/ir.rs` の `Instr { op, cond, rd, rn, imm }`、1 IR 命令 = 1 ARM 命令）で、
  物理 4KB ページ単位のデコードキャッシュ（`arm/code.rs`）に載る。実行中のページ内は
  `Cpu::run_page` が続けて実行する。コードページへの書き込みは MMU の書き込み保護で
  検出し、変換の変化（世代）は `code_cur_va` の無効化で伝わる。
- 速度（起動 4 億命令・boot-1200M）: ネイティブ約 100M 命令/秒、Node の wasm 約 70M。
  ブラウザは 4-1 の前の計測で Firefox 91M・Edge 84M・Safari(Mac) 107M・
  iPhone(A19 Pro) 133M。実時間は 135.2M 命令/秒。ネイティブの律速は命令の振り分け
  そのもので、インタプリタのままでは大きく伸びない（計画書の段階4 の調査）。
- 一致確認: `CERULEAN_IMAGE=$PWD/tmp/images/PPC_USA.bin tools/golden/verify.sh`
  （ネイティブ）と、同じコマンドに `GOLDEN_RUNNER=wasm`（Node で wasm。
  tools/golden/run-wasm.mjs）。事前に `tools/web-build.sh`。計測は tools/bench/bench.sh
  （ネイティブ）と `node tools/web-bench.mjs`（wasm）、ブラウザは tools/serve-bench.py で
  rust/web/www/bench を配信してユーザーに計ってもらう。コミット前に tools/check.sh。

## 今回のゴール: 段階5 の設計案を作ってユーザーの確認を取り、最小の JIT を動かす
1. 実装の前に、次をまとめて質問・提案する（計画書 §8・§9 の「判断が必要」）:
   - wasm のバイナリを作る方法: wasm-encoder クレート（依存の追加）か自作の小さな
     エンコーダか。「汎用のコード生成ライブラリは既存エミュレータのコードに当たらない」
     という解釈の再確認（計画書 §8 で段階5 の前に確認すると約束している）。
   - 生成したモジュールの読み込み: web クレートから JS（WebAssembly.Module・Instance）を
     呼ぶ方法（wasm-bindgen の inline JS 等で依存を増やさずにできるか。js-sys が要るなら
     承認を取る）。コアは std のみ・プラットフォーム非依存のまま、生成（IR → wasm の
     バイト列）はコアかコアの隣のクレート、読み込みと呼び出しは web に置く案。
   - 状態の置き場所: 生成コードがコアの線形メモリ（CPU のレジスタ・CPSR・ソフト TLB・
     ゲスト RAM のアリーナ・RunCtl）を直接読み書きするための配置（repr(C) と
     オフセットの固定、アドレスの受け渡し）。CLAUDE.md の unsafe の規則
     （SAFETY コメント・計測で効果を確かめる・テストで守る）に沿った形。
   - 生成コードからコアへの呼び出し（MMIO・TLB ミス・CP15・例外・未実装命令）の境界と、
     戻った後に割り込み線・上限（RunCtl の budget。LimitRun 相当）・Thumb・
     code_cur_va を確かめる規則。
   - 単位と発火条件: どの単位（基本ブロック・ページ内のトレース）を何回実行したら
     コンパイルするか、何ブロックを 1 モジュールにまとめるか、上限と捨て方
     （メモリ予算、iOS Safari）。コードページの書き換え・世代変化での無効化。
   - 命令数の正確さ: ブロックの途中でも正確な命令数で止まれること（上限・期限・MMIO・
     割り込み・アボート）。ブロックに入る前に残りの上限とブロック長を比べるか、
     命令ごとに数えるか。
   - 使う wasm の機能（ブラウザの対応状況。Safari/iOS を含む。末尾呼び出し・
     例外処理などを使うか）。
   - 検証の方法: JIT とインタプリタの差分テスト（ランダムな命令列・状態）を Node で
     回す仕組み（wasm32-wasip1 のテストは JS のホストがなく JIT を読み込めない点に注意）、
     基準シナリオの一致（GOLDEN_RUNNER=wasm で JIT を有効にして）、trace-hash の突き合わせ。
2. 確認が取れたら、最小の範囲から作る: 特化済みの単純な Op（データ処理・LDR/STR の
   即値・分岐）だけのブロックを JIT し、それ以外はインタプリタに戻す。JIT の有無は
   切り替えられるようにし、無効時の動作と速度が今と変わらないこと。
3. 各段階で、基準シナリオの完全一致（Node の wasm）と計測を行い、計画書・CLAUDE.md・
   README を更新してコミットする。

## 進め方の約束（CLAUDE.md・計画書が正）
- 大きな設計判断・依存の追加は実装前に確認を取る。仕様が不確かな箇所は推測で
  埋めず TODO を残して質問する。既存エミュレータのコードは参照しない。
- 機能ごとにテストを書く。停止のたびに短く報告する。
- 大きな出力は tmp/ に置く（/tmp は小さい tmpfs）。
- コミットは論理単位で、メッセージの末尾にセッションの帰属表示を付ける。
