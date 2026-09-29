# 段階3（ブラウザ版の最小製品）の開始用プロンプト

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。

---

CErulean（WM5 LLE エミュレータ、Rust 製）の段階3（ブラウザ版の最小製品、MVP）を始めたい。
計画は docs/rust-migration-plan.md（特に §3.4・§6.2・§7 全体・§9 の段階3・§12 の未決事項）、
設計方針・Rust の規則・確認済みの事実は CLAUDE.md、現状は README を参照。
まず計画書と CLAUDE.md を最後まで読み、rust/web/src/lib.rs（今の wasm の API）、
rust/web/www/bench（計測ページ: Worker・bench-core.js）、rust/web/www/legacy-serve
（Go 版の serve の UI。入力の対応表と座標変換をここから移す）、rust/core/src/emu.rs・
script.rs・snapshot.rs・smdk2410（入力 API・run_until）、tools/web-build.sh・
tools/serve-bench.py を必要なだけ読んでから始めること。

## 現状（2026-09-29 時点）
- 段階0〜2、段階4（4-1・4-2）、段階5 の 5-1〜5-3 と 5-4 の前半が完了。
- wasm の API（rust/web/src/lib.rs の `Emu`）は計測・一致確認用の最小のもの:
  loadImage・loadSnapshot・saveSnapshot・scheduleScript・run(命令数)・steps・
  setIdleSkip・setJit・jitStats・takeUart・cpuDump・ram・frame（RGBA）。
  入力（touch_down/up・key_down/up）はコアの Machine にあり、今はスクリプト経由でだけ
  渡せる。段階3 で Worker 用の API（入力・実時間との同期・自動保存など）を足す（§7.2）。
- 速度: JIT-to-wasm（段階5）ありで Chrome 155（Mac）が boot-1200M 378M 命令/秒・
  メモリ約 222MB、iPad の Safari 26.2 が約 570M 命令/秒（安定区間）。JIT-to-wasm なしでも
  ブラウザは 84〜133M 命令/秒（段階2）。実時間は 135.2M 命令/秒。
- **既知の問題**: Mac の Safari 27・iPhone（Safari 27.2）で、ページの JIT 全体（素の JS も）が
  効かず 3〜4M 命令/秒になる状態がたびたび起きる。原因は未特定で後回しにした
  （計画書の段階5「ブラウザでの JIT」）。UI では、遅い状態を検出して知らせられるとよい
  （計測ページの JIT 診断と同じ方法: 素の JS のループの時間）。
- 計測ページ（rust/web/www/bench）は tools/serve-bench.py で配信している（キャッシュ無効。
  iPhone は LAN の IP の http で開くので、crypto.subtle・OPFS が使えない。安全な
  コンテキストが要る機能は、localhost か HTTPS で確かめる）。
- 一致確認: `CERULEAN_IMAGE=$PWD/tmp/images/PPC_USA.bin tools/golden/verify.sh`
  （ネイティブ）と、`GOLDEN_RUNNER=wasm`（Node）、`CERULEAN_JIT=1`（JIT あり）。
  コミット前に tools/check.sh。

## 今回のゴール
1. 実装の前に、計画書の「判断が必要」をまとめて質問・提案する（§9 段階3・§12）:
   - 配信先と公開範囲（手元だけか公開サイトか。CSP の `wasm-unsafe-eval`、HTTPS）
   - 対応ブラウザの最低版数（特に iOS/iPadOS の Safari。OPFS の同期アクセスハンドル・
     Web Locks・CompressionStream の対応版数を一次情報で確かめてから提案する）
   - UI の言語、タッチ端末のハードウェアボタンの配置と横向き対応
   - 自動保存の頻度と世代数、差分スナップショットの要否（スナップショットは無圧縮で
     約 134MB、gzip で約 25MB。保存 0.3〜1 秒・gzip 0.6〜1.1 秒。計画書の段階2 の計測）
   - 再開後のゲストの時計の扱い、記録中の kill の扱い
   - .msi からの直接取り出しをするか（しないなら抽出済みの .bin を読む前提）
   - JIT-to-wasm を既定で有効にするか（無効時・失敗時はインタプリタに戻る）
   - 依存を足すなら承認を取る（今は web の wasm-bindgen のみ。UI にフレームワークを
     使うかどうかも含めて。素の HTML/JS で足りるなら依存なし）
2. 確認が取れたら、動くものを小さく積み上げる（各段階で動作を確かめてコミットする）:
   a. Worker＋画面表示: イメージを選んで起動し、canvas に画面を出す。実時間との同期
      （§3.4: 小さな単位で進め、進みすぎたら待ち、遅れたら追いつくのを諦める。Worker は
      1 単位ごとに制御を返す）。状態表示（命令数・仮想時間・実時間比・命令/秒）。
   b. 入力: タッチ（表示座標 → 240×320）・PC のキー（KeyboardEvent.code、IME の
      isComposing 中は送らない）・画面外のハードウェアボタン。入力は命令境界で渡す。
   c. 保存: イメージを SHA-256 をキーに OPFS に保存。スナップショットの保存・読み込み・
      書き出し・読み込み。原子的な保存と壊れたものの除外（§7.3）。
   d. 自動保存と再開（§7.4）: 定期保存・visibilitychange/pagehide、非表示中は止める、
      起動時に「続きから再開」、Web Locks で 1 タブだけ、panic・Worker の異常終了の検出。
   e. 記録と書き出し: 起点スナップショット＋絶対命令数のスクリプト。書き出した
      スクリプトをネイティブ CLI で再生して同じ画面になること（完了条件）。
   f. PWA 化（manifest・Service Worker、更新は自動保存の後に適用）。
3. 完了条件（§9 段階3）: PC の 3 ブラウザと iOS Safari で、起動（または再開）→ 操作 →
   記録 → 書き出したスクリプトをネイティブ CLI で再生して同じ画面、と、タブを閉じる・
   リロード・強制終了から自動保存で再開できること。ブラウザでの確認はユーザーに依頼する
   （iPhone は実機を直接操作する。iPhone ミラーリング経由だと JIT が効かない）。

## 進め方の約束（CLAUDE.md・計画書が正）
- 大きな設計判断・依存の追加は実装前に確認を取る。仕様が不確かな箇所は推測で
  埋めず TODO を残して質問する。既存エミュレータのコードは参照しない。
- コアの動作（ゲストから見える動作）は変えない（UI・API の追加だけ）。変えるなら
  基準を作り直して理由をコミットに残す。コアに足す API は決定論性を保つ（時刻は
  命令数から。壁時計はフロントエンドだけが見る）。
- 機能ごとにテストを書く（コアの API はネイティブのテスト、wasm の API は Node の
  スモークテスト）。停止のたびに短く報告する。
- 大きな出力は tmp/ に置く（/tmp は小さい tmpfs）。
- コミットは論理単位で、メッセージの末尾にセッションの帰属表示を付ける。
