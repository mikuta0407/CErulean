// run-wasm.mjs <シナリオ名> <出力.jsonl>
//
// run.sh の wasm 版: testdata/golden/scenarios/<シナリオ名>.scenario の定義どおりに
// web クレートの wasm（Node 用の出力 rust/web/pkg-node。tools/web-build.sh で作る）を
// リセットから走らせ、結果の JSON Lines（testdata/golden/README.md）を書く。
// ネイティブの CLI の --result と同じ値になるはず（計画書 §9 段階2）。
// 実イメージのシナリオは image=$<環境変数名> の環境変数（CERULEAN_IMAGE など）が必要。
// CERULEAN_JIT=1（または「閾値,まとめる数」。例 1,1）で JIT を有効にする（段階5。
// どの値でも結果は同じになるはず）。
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const require = createRequire(import.meta.url);
const web = require(resolve(root, "rust/web/pkg-node/cerulean_web.js"));

const [name, out] = process.argv.slice(2);
if (!name || !out) {
  console.error("usage: run-wasm.mjs <scenario> <out.jsonl>");
  process.exit(2);
}
const gdir = resolve(root, "testdata/golden");

// シナリオの定義（キー=値、# 以降はコメント）。
const def = {};
for (const raw of readFileSync(`${gdir}/scenarios/${name}.scenario`, "utf8").split("\n")) {
  const line = raw.split("#")[0].trim();
  if (!line) continue;
  const i = line.indexOf("=");
  def[line.slice(0, i)] = line.slice(i + 1).trim();
}
let image = def.image;
if (/^\$[A-Z0-9_]+$/.test(image)) {
  const env = image.slice(1);
  image = process.env[env];
  if (!image) {
    console.error(`run-wasm: ${name} needs ${env}`);
    process.exit(3);
  }
} else {
  image = `${gdir}/${image}`;
}
const imageData = readFileSync(image);
const sha256 = (b) => createHash("sha256").update(b).digest("hex");
if (def.image_sha256 && sha256(imageData) !== def.image_sha256) {
  console.error(`run-wasm: ${image}: sha256 mismatch`);
  process.exit(3);
}
const m = /^(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)$/.exec(def.rtc);
const rtc = Int32Array.from(m.slice(1).map(Number));
const maxSteps = BigInt(def.max_steps);
const checkpoints = (def.checkpoints || "").split(/\s+/).filter(Boolean).map(BigInt).sort((a, b) => (a < b ? -1 : 1));

web.installPanicHook();
const emu = new web.Emu();
if (def.screen) emu.setScreen(...def.screen.split("x").map(Number));
emu.loadImage(imageData, image, rtc);
const jitEnv = process.env.CERULEAN_JIT;
if (jitEnv && jitEnv !== "0") {
  const [threshold, batch] = jitEnv === "1" ? [64, 32] : jitEnv.split(",").map(Number);
  emu.setJit(true, threshold, batch);
}
if (def.script) emu.scheduleScript(readFileSync(`${gdir}/scenarios/${def.script}`, "utf8"));

const uart = createHash("sha256");
let uartBytes = 0;
const drain = () => {
  const b = emu.takeUart();
  uart.update(b);
  uartBytes += b.length;
};
const hex = (b) => Buffer.from(b).toString("hex");
const lines = [];
const record = (event, stop) => {
  drain();
  const dump = emu.cpuDump();
  const screen = emu.frame();
  const [w, h] = [emu.frameWidth(), emu.frameHeight()];
  const stopPart = stop ? `,"stop":${stop}` : "";
  lines.push(
    `{"format":1,"event":"${event}","steps":${emu.steps()}${stopPart},"cpu":"${hex(dump)}",` +
      `"cpu_sha256":"${sha256(dump)}","ram_sha256":"${sha256(emu.ram())}",` +
      `"uart1_sha256":"${uart.copy().digest("hex")}","uart1_bytes":${uartBytes},` +
      `"screen_sha256":"${screen.length ? sha256(screen) : ""}","screen_w":${w},"screen_h":${h}}`,
  );
};

const started = performance.now();
let stop = `{"kind":"max-steps"}`;
for (const target of [...checkpoints.filter((c) => c < maxSteps), maxSteps]) {
  const r = emu.run(target);
  drain();
  if (r !== "ok") {
    stop = emu.stopJson();
    break;
  }
  if (target !== maxSteps) record("checkpoint");
}
const el = (performance.now() - started) / 1000;
record("stop", stop);
writeFileSync(out, lines.join("\n") + "\n");
const n = Number(emu.steps());
console.error(
  `run-wasm: ${name}: ${n} steps in ${el.toFixed(2)}s (${(n / el / 1e6).toFixed(1)}M steps/s, ` +
    `idle-skipped ${((100 * Number(emu.idleSkipped())) / Math.max(n, 1)).toFixed(1)}%, ` +
    `code pages ${emu.codePages()})`,
);
if (jitEnv && jitEnv !== "0") console.error(`run-wasm: ${name}: jit ${emu.jitStats()}`);
