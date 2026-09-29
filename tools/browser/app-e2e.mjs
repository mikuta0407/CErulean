// app-e2e.mjs: ブラウザ版の段階3 の完了条件を Chrome で通しで確かめる（計画書 §9 段階3）。
//
//   tools/web-build.sh && tools/serve-bench.py &
//   node tools/browser/app-e2e.mjs <イメージ> <出力ディレクトリ> [--desktop] [--headed]
//
//  1. イメージから起動 → Today に着いたら自動保存されること
//  2. 記録開始 → タップ・ハードウェアボタン・PC のキーで操作 → 記録停止 → スクリプトと
//     起点スナップショットを書き出す（ダウンロード）
//  3. リロード → 「続きから再開」で再開できること
//  4. Chrome を強制終了（SIGKILL）→ 開き直して再開できること
//  5. 2 つ目のタブでは「別のタブで動いています」になること
//  6. 書き出したスクリプトをネイティブ CLI（rust/target/release/cerulean）で再生し、
//     終わりの CPU 状態・RAM・画面の SHA-256 がスクリプトに書いた値と一致すること
//
// Chrome のプロファイル（OPFS を含む）は出力ディレクトリの下に毎回作り直す。
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, connect, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

const args = process.argv.slice(2);
const [image, outArg] = args.filter((a) => !a.startsWith("--"));
const desktop = args.includes("--desktop");
const headed = args.includes("--headed");
const url = process.env.APP_URL ?? "http://localhost:8000/rust/web/www/app/";
if (!image || !outArg) {
  console.error("usage: app-e2e.mjs <image> <outdir> [--desktop] [--headed]");
  process.exit(2);
}
const outDir = resolve(outArg);
const dlDir = resolve(outDir, "downloads");
const profile = resolve(outDir, "profile");
rmSync(outDir, { recursive: true, force: true });
mkdirSync(dlDir, { recursive: true });
const root = resolve(import.meta.dirname, "../..");
const cli = resolve(root, "rust/target/release/cerulean");

let chrome = null;
async function open() {
  chrome = await launchChrome({ chrome: process.env.CHROME ?? "google-chrome", profile, headless: !headed });
  const p = await openPage(chrome.port);
  echoConsole(p);
  await p.send("Runtime.enable");
  await p.send("Page.enable");
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
  return { p, h };
}

const check = (cond, msg) => {
  if (!cond) throw new Error("FAILED: " + msg);
  console.log("ok:", msg);
};
const endStepOf = (script) => Number(/# end step: (\d+)/.exec(script)[1]);
const logHas = async (h, re) => re.test((await h.status()).log);

// 再開: 「続きから再開」を押して、再開後の命令数を返す
async function resume(p, h, what) {
  await h.waitFor(`${what}: resume button`, () => h.visible("resume"), 20_000);
  const info = await p.eval(`document.getElementById("resumeInfo").textContent`);
  await h.click("resume");
  await h.waitFor(`${what}: resumed`, () => p.eval(`document.getElementById("start").hidden`), 60_000);
  await sleep(3000);
  const s = await h.status();
  console.log(`${what}: resumed from ${info}, now ${s.line}`);
  return s.steps;
}

let failed = false;
try {
  let { p, h } = await open();
  await h.shot("start");

  // 1. 起動
  const { root: doc } = await p.send("DOM.getDocument");
  const { nodeId } = await p.send("DOM.querySelector", { nodeId: doc.nodeId, selector: "#imageFile" });
  await p.send("DOM.setFileInputFiles", { nodeId, files: [resolve(image)] });
  const t0 = Date.now();
  await h.waitFor("boot to Today and autosave", async () => {
    const s = await h.status();
    console.log(`${((Date.now() - t0) / 1000).toFixed(0)}s ${s.line}`);
    await sleep(2000);
    return /保存 auto-0/.test(s.log);
  });
  check((await h.status()).steps >= 3_600_000_000, "Today まで起動して自動保存した");
  await sleep(2000);
  await h.shot("today");

  // 2. 記録と操作
  await h.click("rec");
  await h.waitFor("recording", () => h.visible("recBadge"), 30_000);
  await sleep(1000);
  await h.tap(12, 8); // Start
  await sleep(3000);
  await h.hw("Down");
  await sleep(600);
  await h.hw("Down");
  await sleep(600);
  await h.hw("Enter"); // Contacts
  await sleep(5000);
  await h.key("KeyA", "a", 65);
  await h.key("KeyB", "b", 66);
  await sleep(2000);
  await h.shot("recorded-ops");
  await h.click("rec");
  await h.waitFor("recording stopped", () => h.visible("recText"), 60_000);
  const script = await p.eval(`document.getElementById("recText").textContent`);
  check(/key down A/.test(script) && /key down Down/.test(script) && /down \d+ \d+/.test(script), "記録にタップ・ボタン・キーが入った");
  await h.click("recScript");
  await h.click("recSnap");
  await h.waitFor("downloads", () => {
    const f = readdirSync(dlDir);
    return f.some((n) => n.endsWith(".txt")) && f.some((n) => n.endsWith(".snap.gz"));
  }, 60_000);
  await sleep(2000); // ダウンロードの書き終わりを待つ
  check(true, `書き出した: ${readdirSync(dlDir).join(", ")}`);

  // 操作の後の定期の自動保存（入力があれば 30 秒ごと）を待つ
  await h.waitFor("periodic autosave", () => logHas(h, /保存 auto-1/), 60_000);
  const periodic = Number(/保存 auto-1: 命令 ([\d,]+)/.exec((await h.status()).log)[1].replace(/,/g, ""));
  check(periodic > 3_600_000_000, `定期の自動保存をした（命令 ${periodic}）`);
  // 手動保存
  await h.click("saveNow");
  await h.waitFor("manual save", () => logHas(h, /保存 save-/), 60_000);
  const manualSteps = Number(/保存 save-\d+: 命令 ([\d,]+)/.exec((await h.status()).log)[1].replace(/,/g, ""));
  console.log((await h.status()).log.split("\n").filter((l) => l.includes("保存")).join("\n"));
  const beforeReload = (await h.status()).steps;
  await h.shot("before-reload");

  // 3. リロードから再開
  await p.send("Page.reload");
  await h.waitFor("reloaded", () => p.eval(`document.getElementById("startMsg") && document.getElementById("startMsg").textContent !== "準備中…"`).catch(() => false), 20_000);
  const r1 = await resume(p, h, "reload");
  check(r1 >= manualSteps, `リロード後にいちばん新しい保存（命令 ${manualSteps} 以降）から再開した（リロード時 ${beforeReload}）`);
  await h.shot("after-reload");

  // 4. 強制終了から再開
  await sleep(2000);
  chrome.proc.kill("SIGKILL");
  await sleep(1000);
  ({ p, h } = await open());
  const r2 = await resume(p, h, "kill");
  check(r2 >= manualSteps, "強制終了の後に保存から再開した");
  await h.shot("after-kill");

  // 5. 2 つ目のタブ
  const t = await (await fetch(`http://127.0.0.1:${chrome.port}/json/new?${encodeURIComponent(url)}`, { method: "PUT" })).json();
  const p2 = await connect(t.webSocketDebuggerUrl);
  await p2.send("Runtime.enable");
  const h2 = appHelpers(p2, outDir);
  await h2.waitFor("second tab blocked", () => h2.visible("otherTab").catch(() => false), 20_000);
  check(true, "2 つ目のタブは「別のタブで動いています」になった");
  p2.close();

  // 6. CLI で再生
  if (!existsSync(cli)) throw new Error(`${cli} がない（cd rust && cargo build --release）`);
  const txt = readdirSync(dlDir).find((n) => n.endsWith(".txt"));
  const gz = readdirSync(dlDir).find((n) => n.endsWith(".snap.gz"));
  execFileSync("gunzip", ["-kf", resolve(dlDir, gz)]);
  const result = resolve(outDir, "result.jsonl");
  execFileSync(cli, ["run", "--quiet-uart", "--snap-load", resolve(dlDir, gz.replace(/\.gz$/, "")), "--script", resolve(dlDir, txt), "--result", result, resolve(image)], {
    cwd: outDir,
    stdio: ["ignore", "inherit", "inherit"],
  });
  const last = JSON.parse(readFileSync(result, "utf8").trim().split("\n").at(-1));
  const want = Object.fromEntries([...script.matchAll(/(\w+_sha256) ([0-9a-f]{64})/g)].map((m) => [m[1], m[2]]));
  const endStep = endStepOf(script);
  check(last.steps === endStep, `CLI の再生が記録の終わりの命令数 ${endStep} で止まった`);
  for (const k of ["cpu_sha256", "ram_sha256", "screen_sha256"]) check(last[k] === want[k], `CLI の再生で ${k} が一致`);
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  if (!headed) chrome?.proc.kill();
}
console.log(failed ? "app-e2e: FAILED" : "app-e2e: ok");
process.exit(failed ? 1 : 0);
