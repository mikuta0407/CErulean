// jit-diff.mjs <wasm-bindgen の --target nodejs の出力ディレクトリ> [seed] [件数] [命令数]
// JIT とインタプリタの差分テスト（core の jit::selftest）を Node で走らせる。
// wasm32-wasip1 のテストは JS のホストがなく JIT を読み込めないので、ここで回す
// （tools/check.sh から呼ぶ）。
import { createRequire } from "node:module";
import { resolve } from "node:path";

const require = createRequire(import.meta.url);
const web = require(resolve(process.argv[2], "cerulean_web.js"));
web.installPanicHook();
const seed = BigInt(process.argv[3] ?? 1);
const cases = Number(process.argv[4] ?? 1000);
const steps = BigInt(process.argv[5] ?? 4000);
const t0 = performance.now();
const r = web.jitSelfTest(seed, cases, steps);
console.log(`jit-diff: ${r} (${((performance.now() - t0) / 1000).toFixed(1)}s)`);
if (r.startsWith("FAIL")) process.exit(1);
