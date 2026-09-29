// app-smoke.mjs: ブラウザ版（rust/web/www/app）を Chrome で開き、イメージから起動して
// Today まで進め、タッチ・ハードウェアボタン・PC のキーで操作して画面を PNG に残す。
//
//   tools/web-build.sh && tools/serve-bench.py &
//   node tools/browser/app-smoke.mjs <イメージ> <出力ディレクトリ> [--headed] [--desktop]
//     [--viewport=幅x高さ] [--layout-only]
//
// 既定はスマートフォンの縦向き（390×844・DPR 3・タッチ）を真似る。--desktop は
// 1280×800・マウス。--layout-only は起動前の画面だけ撮って終わる（配置の確認）。Chrome は
// google-chrome（CHROME で変える）。プロファイルは出力ディレクトリの下に毎回作り直す。
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { launchChrome, openPage } from "./cdp.mjs";

const [image, outDir] = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const headed = process.argv.includes("--headed");
const desktop = process.argv.includes("--desktop");
const layoutOnly = process.argv.includes("--layout-only");
const vp = process.argv.find((a) => a.startsWith("--viewport="))?.slice(11).split("x").map(Number);
const url = process.env.APP_URL ?? "http://localhost:8000/rust/web/www/app/";
if (!image || !outDir) {
  console.error("usage: app-smoke.mjs <image> <outdir> [--headed] [--desktop]");
  process.exit(2);
}
mkdirSync(outDir, { recursive: true });
const profile = resolve(outDir, "profile");
rmSync(profile, { recursive: true, force: true });

const { proc, port } = await launchChrome({ chrome: process.env.CHROME ?? "google-chrome", profile, headless: !headed });
const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));
let failed = false;
try {
  const p = await openPage(port);
  p.on((m) => {
    if (m.method === "Runtime.consoleAPICalled") console.log("console:", m.params.args.map((a) => a.value ?? a.description).join(" "));
    if (m.method === "Runtime.exceptionThrown") console.log("exception:", JSON.stringify(m.params.exceptionDetails).slice(0, 500));
  });
  await p.send("Runtime.enable");
  await p.send("Page.enable");
  if (!desktop) {
    await p.send("Emulation.setDeviceMetricsOverride", { width: vp?.[0] ?? 390, height: vp?.[1] ?? 844, deviceScaleFactor: 3, mobile: true });
    await p.send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
  } else {
    await p.send("Emulation.setDeviceMetricsOverride", { width: vp?.[0] ?? 1280, height: vp?.[1] ?? 800, deviceScaleFactor: 1, mobile: false });
  }
  await p.send("Page.navigate", { url });
  await sleep(1500);

  let shotN = 0;
  const shot = async (name) => {
    const r = await p.send("Page.captureScreenshot", { format: "png" });
    const f = resolve(outDir, `${String(++shotN).padStart(2, "0")}-${name}.png`);
    writeFileSync(f, Buffer.from(r.data, "base64"));
    console.log("shot", f);
  };
  const status = () => p.eval(`({steps: document.getElementById("steps").textContent, line: document.getElementById("statusLine").textContent, log: document.getElementById("log").textContent.slice(0, 300)})`);
  await shot("start");
  if (layoutOnly) throw "layout-only";

  // イメージを選ぶ
  const { root } = await p.send("DOM.getDocument");
  const { nodeId } = await p.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#imageFile" });
  await p.send("DOM.setFileInputFiles", { nodeId, files: [resolve(image)] });

  // Today まで（起動の早送りが終わるまで）待つ
  const t0 = Date.now();
  for (;;) {
    await sleep(2000);
    const s = await status();
    const steps = Number(s.steps.replace(/,/g, "")) || 0;
    console.log(`${((Date.now() - t0) / 1000).toFixed(0)}s ${s.line} steps=${s.steps}`);
    if (steps >= 3_700_000_000) break;
    if (Date.now() - t0 > 300_000) throw new Error("boot timeout: " + JSON.stringify(s));
  }
  await sleep(3000);
  await shot("today");

  // 画面座標（240×320）→ ページの座標
  const rect = await p.eval(`(() => { const r = document.getElementById("screen").getBoundingClientRect(); return {x: r.left, y: r.top, w: r.width, h: r.height}; })()`);
  const at = (gx, gy) => ({ x: rect.x + ((gx + 0.5) * rect.w) / 240, y: rect.y + ((gy + 0.5) * rect.h) / 320 });
  const tap = async (gx, gy) => {
    const pt = at(gx, gy);
    if (desktop) {
      await p.send("Input.dispatchMouseEvent", { type: "mousePressed", ...pt, button: "left", clickCount: 1 });
      await sleep(150);
      await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...pt, button: "left", clickCount: 1 });
    } else {
      await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [pt] });
      await sleep(150);
      await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    }
  };
  const hw = async (key) => {
    const r = await p.eval(`(() => { const r = document.querySelector('[data-key="${key}"]').getBoundingClientRect(); return {x: r.left + r.width / 2, y: r.top + r.height / 2}; })()`);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [r] });
    await sleep(120);
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  };
  const key = async (code, key, keyCode) => {
    await p.send("Input.dispatchKeyEvent", { type: "rawKeyDown", code, key, windowsVirtualKeyCode: keyCode });
    await sleep(80);
    await p.send("Input.dispatchKeyEvent", { type: "keyUp", code, key, windowsVirtualKeyCode: keyCode });
    await sleep(80);
  };

  // 1. タップ: 左上の Start を押してメニューを出す
  await tap(12, 8);
  await sleep(4000);
  await shot("tap-start");
  // 2. ハードウェアボタン: 下・下・決定（メニューの項目を選んで開く）
  await hw("Down");
  await sleep(700);
  await hw("Down");
  await sleep(700);
  await shot("hw-down");
  await hw("Enter");
  await sleep(6000);
  await shot("hw-enter");
  // 3. PC のキー: 矢印と英字
  await key("ArrowDown", "ArrowDown", 40);
  await sleep(1000);
  await key("KeyA", "a", 65);
  await key("KeyB", "b", 66);
  await sleep(3000);
  await shot("keys");
  console.log(JSON.stringify(await status()));
} catch (e) {
  if (e !== "layout-only") {
    failed = true;
    console.error(e);
  }
} finally {
  if (!headed) proc.kill();
}
process.exit(failed ? 1 : 0);
