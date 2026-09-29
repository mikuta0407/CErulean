// jit-mem.mjs <pkg-node のディレクトリ> <命令数> [閾値,まとめる数]
// node --expose-gc で走らせる。リセットから命令数まで区切って走らせ、区切りごとに GC の
// 後の RSS・V8 のヒープ・JIT の生成量を出し、最後に最大の RSS を「peak rss <MB>」の形で
// 出す（段階5-4。JIT ありで V8 のメモリがどれだけ増えるかを見る）。hwm は途中も含めた
// RSS の最大値。glibc の malloc は解放した領域を OS に返さずに持つことが多く、RSS は
// V8 が今使っている量より大きく出る（MALLOC_TRIM_THRESHOLD_=131072
// MALLOC_MMAP_THRESHOLD_=131072 を付けると、大きな作業領域が返されて差が見える）。jit を省略すると
// JIT なし。実イメージは CERULEAN_IMAGE（既定 tmp/images/PPC_USA.bin）。
// 区切りの数は CERULEAN_MEM_STEPS（既定 6）。
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (typeof gc !== "function") {
  console.error("run with node --expose-gc");
  process.exit(2);
}
const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const [pkg, steps, jit] = process.argv.slice(2);
const web = createRequire(import.meta.url)(resolve(pkg, "cerulean_web.js"));
const image = process.env.CERULEAN_IMAGE ?? resolve(root, "tmp/images/PPC_USA.bin");
const parts = Number(process.env.CERULEAN_MEM_STEPS ?? 6);
const emu = new web.Emu();
emu.loadImage(readFileSync(image), image, Int32Array.from([2006, 1, 2, 15, 4, 5]));
if (jit) {
  const [t, b] = jit.split(",").map(Number);
  emu.setJit(true, t, b);
}
const mb = (v) => (v / 1048576).toFixed(0);
let peak = 0;
const t0 = performance.now();
for (let i = 1; i <= parts; i++) {
  emu.run((BigInt(steps) * BigInt(i)) / BigInt(parts));
  gc();
  const m = process.memoryUsage();
  peak = Math.max(peak, m.rss);
  let js = "";
  if (jit) {
    const s = JSON.parse(emu.jitStats());
    js = ` jit-bytes ${mb(s.bytes)}MB pages ${s.pages} modules ${s.modules} max-func ${(s.max_func / 1024).toFixed(0)}KB`;
    if (s.error) js += ` error ${s.error}`;
  }
  console.log(
    `${emu.steps()} rss ${mb(m.rss)}MB heap ${mb(m.heapUsed)}MB ext ${mb(m.external)}MB${js}`,
  );
}
const el = (performance.now() - t0) / 1000;
// VmHWM: 途中（コンパイル中など）も含めた RSS の最大値（Linux のみ）
let hwm = "";
try {
  const m = readFileSync("/proc/self/status", "utf8").match(/VmHWM:\s*(\d+) kB/);
  if (m) hwm = `  hwm ${(Number(m[1]) / 1024).toFixed(0)}MB`;
} catch {}
console.log(`peak rss ${mb(peak)}MB${hwm}  (${el.toFixed(1)}s)`);
