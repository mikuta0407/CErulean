// web-bench.mjs: ブラウザの計測ページ（rust/web/www/bench）と同じ計測を Node で行う。
// 使い方: tools/web-build.sh の後、CERULEAN_IMAGE=... node tools/web-bench.mjs
//   [シナリオ（boot-1200M・boot-today、既定 boot-1200M）] [JIT の「閾値,数」（省略で JIT なし）]
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const www = resolve(root, "rust/web/www");
const wasm = await import(resolve(www, "pkg/cerulean_web.js"));
const { runBench } = await import(resolve(www, "bench/bench-core.js"));
await wasm.default({ module_or_path: readFileSync(resolve(www, "pkg/cerulean_web_bg.wasm")) });
wasm.installPanicHook();
const [scenario = "boot-1200M", jitArg] = process.argv.slice(2);
const jit = jitArg ? jitArg.split(",").map(Number) : null;
const lines = readFileSync(resolve(root, `testdata/golden/expected/${scenario}.jsonl`), "utf8").trim().split("\n");
const expected = JSON.parse(lines.at(-1));
const image = process.env.CERULEAN_IMAGE ?? resolve(root, "tmp/images/PPC_USA.bin");
const result = await runBench({ wasm, imageBytes: new Uint8Array(readFileSync(image)), expected, log: console.error, jit, snapshot: !jit });
console.log(JSON.stringify(result, null, 1));
