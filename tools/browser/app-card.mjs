// app-card.mjs: ブラウザ版のストレージカードを Chrome で通しで確かめる
// （docs/storage-card-design.md。2026-09-29 ユーザー決定の「カードに簡単にデータを入れられる」）。
//
//   tools/web-build.sh && tools/serve-bench.py &
//   node tools/browser/app-card.mjs <イメージ> <出力ディレクトリ> [--desktop] [--headed]
//
//  1. イメージから起動 → Today に着いて自動保存されること
//  2. メニューで空のカードを作り、ホストのファイル（日本語・長い名前を含む）を入れると
//     一覧に出ること
//  3. 記録開始 → カードを挿す → File Explorer で Storage Card を開く → 抜く → 記録停止。
//     抜いた後の一覧にファイルが残っていること
//  4. 書き出したスクリプト・起点スナップショット・挿したカードのイメージをネイティブ CLI で
//     再生し、終わりの CPU 状態・RAM・画面の SHA-256 が一致すること（カードの挿抜が
//     決定論的に記録・再生されること）
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

const args = process.argv.slice(2);
const [image, outArg] = args.filter((a) => !a.startsWith("--"));
const desktop = args.includes("--desktop");
const headed = args.includes("--headed");
const url = process.env.APP_URL ?? "http://localhost:8000/web/www/app/";
if (!image || !outArg) {
  console.error("usage: app-card.mjs <image> <outdir> [--desktop] [--headed]");
  process.exit(2);
}
const outDir = resolve(outArg);
const dlDir = resolve(outDir, "downloads");
const srcDir = resolve(outDir, "src");
const profile = resolve(outDir, "profile");
rmSync(outDir, { recursive: true, force: true });
mkdirSync(dlDir, { recursive: true });
mkdirSync(srcDir, { recursive: true });
const root = resolve(import.meta.dirname, "../..");
const cli = resolve(root, "target/release/cerulean");

const check = (cond, msg) => {
  if (!cond) throw new Error("FAILED: " + msg);
  console.log("ok:", msg);
};

// カードに入れるホストのファイル
const files = {
  "readme.txt": "Hello from the host!\r\n",
  "日本語のファイル名.txt": "こんにちは\r\n",
  "A Long File Name With Spaces.bin": "x".repeat(70_000),
};
for (const [n, v] of Object.entries(files)) writeFileSync(resolve(srcDir, n), v);

let chrome = null;
let failed = false;
try {
  chrome = await launchChrome({ chrome: process.env.CHROME ?? "google-chrome", profile, headless: !headed });
  const p = await openPage(chrome.port);
  echoConsole(p);
  await p.send("Runtime.enable");
  await p.send("Page.enable");
  await p.send("DOM.enable");
  await p.send("Page.setDownloadBehavior", { behavior: "allow", downloadPath: dlDir });
  if (desktop) {
    await p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  } else {
    await p.send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 3, mobile: true });
    await p.send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 5 });
  }
  await p.send("Page.navigate", { url });
  const h = appHelpers(p, outDir, { desktop });
  await h.waitFor("worker ready", () => p.eval(`document.getElementById("startMsg").textContent !== "準備中…"`), 20_000);
  const setFiles = async (selector, paths) => {
    const { root: doc } = await p.send("DOM.getDocument");
    const { nodeId } = await p.send("DOM.querySelector", { nodeId: doc.nodeId, selector });
    await p.send("DOM.setFileInputFiles", { nodeId, files: paths });
  };
  const text = (id) => p.eval(`document.getElementById(${JSON.stringify(id)}).textContent`);
  const cardNames = () => p.eval(`[...document.querySelectorAll("#cardList li button.boot span")].map((s) => s.textContent)`);

  // 1. 起動
  await setFiles("#imageFile", [resolve(image)]);
  const t0 = Date.now();
  await h.waitFor("boot to Today and autosave", async () => {
    const s = await h.status();
    console.log(`${((Date.now() - t0) / 1000).toFixed(0)}s ${s.line}`);
    await sleep(2000);
    return /保存 auto-0/.test(s.log);
  });
  check((await h.status()).steps >= 3_600_000_000, "Today まで起動した");

  // 2. カードを作ってファイルを入れる
  await h.click("menuBtn");
  await sleep(500);
  await p.eval(`document.getElementById("cardSize").value = "32"`);
  await h.click("cardNew");
  await h.waitFor("card created", async () => !(await h.visible("cardFiles")) ? false : /抜いています/.test(await text("cardState")), 30_000);
  await setFiles("#cardAddFiles", Object.keys(files).map((n) => resolve(srcDir, n)));
  await h.waitFor("files listed", async () => (await cardNames()).length === 3, 30_000);
  check(true, `カードにファイルを入れた: ${(await cardNames()).join(" / ")}`);
  await h.shot("card-files");

  // 3. 記録しながら挿して File Explorer で開き、抜く
  await h.click("rec");
  await h.waitFor("recording", () => h.visible("recBadge"), 30_000);
  await h.click("cardInsert");
  await h.waitFor("inserted", async () => /挿しています/.test(await text("cardState")), 30_000);
  check(true, "カードを挿した");
  await h.click("menuBtn"); // メニューを閉じて画面を操作する
  await sleep(4000);
  await h.tap(12, 8); // Start
  await sleep(2000);
  await h.tap(50, 171); // Programs
  await sleep(4000);
  await h.tap(196, 110); // File Explorer
  await sleep(6000);
  await h.tap(43, 306); // Up
  await sleep(3000);
  await h.tap(43, 306); // Up（My Device）
  await sleep(3000);
  await h.tap(60, 147); // Storage Card
  await sleep(4000);
  await h.shot("storage-card");
  await h.click("menuBtn");
  await sleep(500);
  await h.click("cardEject");
  await h.waitFor("ejected", async () => /抜いています/.test(await text("cardState")), 30_000);
  await h.waitFor("files after eject", async () => (await cardNames()).length === 3, 30_000);
  check(true, "抜いた後もファイルが残っている");
  await h.click("rec");
  await h.waitFor("recording stopped", () => h.visible("recText"), 60_000);
  const script = await text("recText");
  check(/\d+i card insert cerulean-card-[0-9a-f]+\.img/.test(script) && /\d+i card eject/.test(script), "記録にカードの挿抜が入った");
  await h.click("recScript");
  await h.click("recSnap");
  await h.click("recCards");
  await h.waitFor("downloads", () => {
    const f = readdirSync(dlDir);
    return f.some((n) => n.endsWith(".txt")) && f.some((n) => n.endsWith(".snap.gz")) && f.some((n) => n.endsWith(".img"));
  }, 60_000);
  await sleep(2000);
  check(true, `書き出した: ${readdirSync(dlDir).join(", ")}`);
  await h.shot("after-eject");

  // 4. CLI で再生（card insert のイメージはスクリプトと同じ場所に置く）
  if (!existsSync(cli)) throw new Error(`${cli} がない（cargo build --release）`);
  const txt = readdirSync(dlDir).find((n) => n.endsWith(".txt"));
  const gz = readdirSync(dlDir).find((n) => n.endsWith(".snap.gz"));
  execFileSync("gunzip", ["-kf", resolve(dlDir, gz)]);
  const result = resolve(outDir, "result.jsonl");
  execFileSync(cli, ["run", "--quiet-uart", "--snap-load", resolve(dlDir, gz.replace(/\.gz$/, "")), "--script", resolve(dlDir, txt), "--result", result, resolve(image)], {
    cwd: dlDir,
    stdio: ["ignore", "inherit", "inherit"],
  });
  const last = JSON.parse(readFileSync(result, "utf8").trim().split("\n").at(-1));
  const want = Object.fromEntries([...script.matchAll(/(\w+_sha256) ([0-9a-f]{64})/g)].map((m) => [m[1], m[2]]));
  const endStep = Number(/# end step: (\d+)/.exec(script)[1]);
  check(last.steps === endStep, `CLI の再生が記録の終わりの命令数 ${endStep} で止まった`);
  for (const k of ["cpu_sha256", "ram_sha256", "screen_sha256"]) check(last[k] === want[k], `CLI の再生で ${k} が一致`);
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  if (!headed) chrome?.proc.kill();
}
console.log(failed ? "app-card: FAILED" : "app-card: ok");
process.exit(failed ? 1 : 0);
