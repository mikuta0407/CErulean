// app-profile.mjs: ブラウザ版のプロファイル（仮想環境の使い分け）を Chrome で確かめる。
//
//   tools/web-build.sh && tools/serve-bench.py &
//   node tools/browser/app-profile.mjs <イメージ A> <イメージ B> <出力ディレクトリ> [--headed]
//
//  1. 既定のプロファイルでイメージ A から起動し、Today（自動保存）まで進める
//  2. 新しいプロファイル「B」を作る → マシンが止まり、最初の画面になる
//  3. イメージ B から起動する（名前に VGA があれば画面は自動で 480×640）
//  4. 既定のプロファイルに切り替える → A の続き（Today の後）から再開する
//  5. B に戻す → B の続きから再開する
//  6. リロードしても今のプロファイル（B）のまま再開できる
//  7. 既定に切り替えて B を消す
//
// Chrome のプロファイル（OPFS を含む）は出力ディレクトリの下に毎回作り直す。
import { mkdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

const args = process.argv.slice(2);
const [imageA, imageB, outArg] = args.filter((a) => !a.startsWith("--"));
const headed = args.includes("--headed");
const url = process.env.APP_URL ?? "http://localhost:8000/web/www/app/";
if (!imageA || !imageB || !outArg) {
  console.error("usage: app-profile.mjs <imageA> <imageB> <outdir> [--headed]");
  process.exit(2);
}
const outDir = resolve(outArg);
mkdirSync(outDir, { recursive: true });
const profile = resolve(outDir, "profile");
rmSync(profile, { recursive: true, force: true });

let failed = false;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failed = true;
};

const { proc, port } = await launchChrome({ chrome: process.env.CHROME ?? "google-chrome", profile, headless: !headed });
try {
  const p = await openPage(port);
  echoConsole(p);
  await p.send("Runtime.enable");
  await p.send("Page.enable");
  await p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  // prompt（名前）・confirm（削除）に答える
  let promptText = "";
  p.on((m) => {
    if (m.method === "Page.javascriptDialogOpening") p.send("Page.handleJavaScriptDialog", { accept: true, promptText });
  });
  await p.send("Page.navigate", { url });
  await sleep(1500);
  const h = appHelpers(p, outDir, { desktop: true });
  const ev = (js) => p.eval(js);
  const canvasW = () => ev(`document.getElementById("screen").width`);
  const profileName = () => ev(`document.getElementById("profileName").textContent`);
  const startShown = () => h.visible("start");
  const setFile = async (file) => {
    const { root } = await p.send("DOM.getDocument");
    const { nodeId } = await p.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#imageFile" });
    await p.send("DOM.setFileInputFiles", { nodeId, files: [resolve(file)] });
  };
  const waitToday = (desc) =>
    h.waitFor(desc, async () => {
      const s = await h.status();
      await sleep(1000);
      return s.steps >= 3_700_000_000 && /保存 auto-\d/.test(s.log.split("\n")[0]) ? s : null;
    }, 600_000);
  const waitResumed = (desc, minSteps) =>
    h.waitFor(desc, async () => !(await startShown()) && (await h.status()).steps >= minSteps, 120_000);
  const select = (id) => ev(`(() => { const s = document.getElementById("profileSel"); s.value = ${JSON.stringify(id)}; s.dispatchEvent(new Event("change")); })()`);
  const profileId = (name) => ev(`[...document.getElementById("profileSel").options].find((o) => o.text === ${JSON.stringify(name)})?.value`);

  // 1. 既定のプロファイルで A
  check((await profileName()) === "既定", "最初は既定のプロファイル");
  await setFile(imageA);
  const a = await waitToday("A が Today に着いて自動保存");
  const aSteps = a.steps;
  await h.shot("a-today");
  check((await canvasW()) === 240, "A の画面は 240 幅");

  // 2. 新しいプロファイル B
  await h.click("toStart");
  await sleep(500);
  promptText = "B";
  await h.click("profileNew");
  await h.waitFor("B に切り替わって最初の画面", async () => (await profileName()) === "B" && (await startShown()), 60_000);
  await sleep(1500);
  check(!(await h.visible("resume")), "新しいプロファイル B では最初の画面（保存がなく「続きから再開」がない）");
  const log0 = (await h.status()).log.split("\n");
  const iSwitch = log0.findIndex((l) => l.includes("プロファイル「B」に切り替えた"));
  // ログは新しい順: 切り替えの行より後ろ（古い側）の直前に自動保存がある
  check(iSwitch >= 0 && /保存 auto-\d/.test(log0.slice(iSwitch + 1).join("\n").split("\n")[0]), "切り替えの前に A を自動保存した");
  await h.shot("b-start");

  // 3. B を起動
  await setFile(imageB);
  const b = await waitToday("B が Today に着いて自動保存");
  const bSteps = b.steps;
  await h.shot("b-today");
  const bw = await canvasW();
  console.log(`B の画面の幅: ${bw}`);
  if (/vga/i.test(imageB)) check(bw === 480, "B（VGA）の画面は 480 幅");

  // 4. 既定に戻す: A の続き
  await h.click("toStart");
  await sleep(500);
  await select("default");
  await waitResumed("既定に切り替えて A の続きから再開", aSteps);
  await sleep(2000);
  check((await profileName()) === "既定" && (await canvasW()) === 240, "既定のプロファイルで A（240 幅）が動いている");
  await h.shot("a-again");

  // 5. B に戻す: B の続き
  const bId = await profileId("B");
  await h.click("toStart");
  await sleep(500);
  await select(bId);
  await waitResumed("B に切り替えて B の続きから再開", bSteps);
  await sleep(2000);
  check((await profileName()) === "B" && (await canvasW()) === bw, "B のプロファイルで B が動いている");

  // 6. リロード: B のまま「続きから再開」
  await p.send("Page.reload");
  await sleep(2000);
  await h.waitFor("一覧", () => h.visible("resume"), 60_000);
  check((await profileName()) === "B", "リロードしても今のプロファイルは B");
  await h.click("resume");
  await waitResumed("B の続きから再開（リロードの後）", bSteps);
  check((await canvasW()) === bw, "リロードの後も B が動いている");
  await h.shot("b-reload");

  // 7. B を消す（既定に切り替えてから消える）
  await h.click("toStart");
  await sleep(500);
  await h.click("profileDel");
  await h.waitFor("B が消える", async () => !(await profileId("B")), 120_000);
  check((await profileName()) === "既定", "消した後は既定のプロファイル");
  await h.shot("deleted");
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  if (!headed) proc.kill();
}
process.exit(failed ? 1 : 0);
