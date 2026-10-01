// jit-diff.mjs <wasm-bindgen の --target nodejs の出力ディレクトリ> [seed の列 (例 1,2)] [件数] [命令数]
// JIT とインタプリタの差分テスト（core の jit::selftest）を Node で走らせる。
// wasm32-wasip1 のテストは JS のホストがなく JIT を読み込めないので、ここで回す
// （tools/check.sh から呼ぶ）。既定は seed 1 と 2 を 1000 件ずつ（約 20 秒）。
import { createRequire } from "node:module";
import { resolve } from "node:path";

const require = createRequire(import.meta.url);
const web = require(resolve(process.argv[2], "cerulean_web.js"));
web.installPanicHook();
const seeds = (process.argv[3] ?? "1,2").split(",").map(BigInt);
const cases = Number(process.argv[4] ?? 1000);
const steps = BigInt(process.argv[5] ?? 4000);
let failed = false;
for (const seed of seeds) {
  const t0 = performance.now();
  const r = web.jitSelfTest(seed, cases, steps);
  console.log(`jit-diff: seed ${seed}: ${r} (${((performance.now() - t0) / 1000).toFixed(1)}s)`);
  failed ||= r.startsWith("FAIL");
}
if (failed) process.exit(1);
