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
import { mkdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

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
let failed = false;
try {
  const p = await openPage(port);
  echoConsole(p);
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

  const h = appHelpers(p, outDir, { desktop });
  const { shot, tap, hw, key } = h;
  await shot("start");
  if (layoutOnly) throw "layout-only";

  // イメージを選ぶ
  const { root } = await p.send("DOM.getDocument");
  const { nodeId } = await p.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#imageFile" });
  await p.send("DOM.setFileInputFiles", { nodeId, files: [resolve(image)] });

  // Today まで（起動の早送りが終わるまで）待つ
  const t0 = Date.now();
  await h.waitFor("boot", async () => {
    const s = await h.status();
    console.log(`${((Date.now() - t0) / 1000).toFixed(0)}s ${s.line} steps=${s.steps}`);
    await sleep(1500);
    return s.steps >= 3_700_000_000;
  });
  await sleep(3000);
  await shot("today");

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
  console.log(JSON.stringify((await h.status()).line));
} catch (e) {
  if (e !== "layout-only") {
    failed = true;
    console.error(e);
  }
} finally {
  if (!headed) proc.kill();
}
process.exit(failed ? 1 : 0);
