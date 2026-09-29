// wasm-run.mjs <pkg-node のディレクトリ> <命令数> [jit]
// web クレートの wasm（Node 用の出力）でリセットから命令数まで走らせ、命令/秒を
// 「<値>M steps/s」の形で出す（web-bench.sh が読む）。jit は "閾値,まとめる数"
// （省略時は JIT なし）。実イメージは CERULEAN_IMAGE（既定 tmp/images/PPC_USA.bin）。
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const [pkg, steps, jit] = process.argv.slice(2);
const web = createRequire(import.meta.url)(resolve(pkg, "cerulean_web.js"));
const image = process.env.CERULEAN_IMAGE ?? resolve(root, "tmp/images/PPC_USA.bin");
const emu = new web.Emu();
emu.loadImage(readFileSync(image), image, Int32Array.from([2006, 1, 2, 15, 4, 5]));
if (jit) {
  const [t, b] = jit.split(",").map(Number);
  emu.setJit(true, t, b);
}
const t0 = performance.now();
emu.run(BigInt(steps));
const el = (performance.now() - t0) / 1000;
const n = Number(emu.steps());
console.log(`${(n / el / 1e6).toFixed(1)}M steps/s (${n} steps in ${el.toFixed(2)}s)`);
if (jit) console.log(`jit ${emu.jitStats()}`);
