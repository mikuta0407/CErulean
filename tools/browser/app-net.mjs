// app-net.mjs: ブラウザ版のネットワーク（中継サーバー経由）を Chrome で通しで確かめる
// （docs/network-design.md）。
//
//   tools/web-build.sh && (cd rust && cargo build --release)
//   node tools/browser/app-net.mjs <イメージ> <スナップショット> <出力ディレクトリ> [--headed]
//
// アプリと中継サーバーは `cerulean serve --with-relay` で配信する（中継の URL はアプリの既定の
// 「同じサイトの /relay」、トークンは表示された URL の #relay-token= で入る）。
//
// スナップショットは JPN 版（PPC_JPN.bin）の Today で、WM5 の「ネットワークカードの接続先」を
// 「インターネット設定」にしたもの（座標は JPN 版の画面）。インターネットに出られること。
//
//  1. cerulean serve --with-relay をトークン付きで起動し、#relay-token= 付きの URL を開いて
//     スナップショットを読み込む
//  2. メニューでオンにするだけで（URL・トークンは既定と URL から）中継につながること
//  3. 記録しながら Internet Explorer で http://example.com/ を開き、中継が接続を
//     中継したこと。続けて http://10.0.2.2/ から CA を入れて https://example.com/ を開き、
//     中継が TLS でつないだこと
//  4. ネットワークをオンにしたままストレージカードを挿すとフォルダ共有の方式になり、File
//     Explorer の Storage Card に見えること
//  5. 書き出したスクリプトと起点スナップショットをネイティブ CLI で（ネットワークなしで）
//     再生し、終わりの CPU 状態・RAM・画面の SHA-256 が一致すること
import { execFileSync, spawn } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

const args = process.argv.slice(2);
const [image, snap, outArg] = args.filter((a) => !a.startsWith("--"));
const headed = args.includes("--headed");
if (!image || !snap || !outArg) {
  console.error("usage: app-net.mjs <image> <snapshot> <outdir> [--headed]");
  process.exit(2);
}
const outDir = resolve(outArg);
const dlDir = resolve(outDir, "downloads");
const profile = resolve(outDir, "profile");
rmSync(outDir, { recursive: true, force: true });
mkdirSync(dlDir, { recursive: true });
const root = resolve(import.meta.dirname, "../..");
const cli = resolve(root, "rust/target/release/cerulean");
const TOKEN = "app-net-test-token";
const RELAY = "127.0.0.1:18765";
const url = `http://${RELAY}/app/#relay-token=${TOKEN}`;

const check = (cond, msg) => {
  if (!cond) throw new Error("FAILED: " + msg);
  console.log("ok:", msg);
};

let relayLog = "";
const relay = spawn(cli, ["serve", "--listen", RELAY, "--with-relay", "--token", TOKEN], { stdio: ["ignore", "ignore", "pipe"] });
await sleep(1000);
relay.stderr.on("data", (d) => {
  relayLog += d;
  process.stdout.write("relay: " + d);
});

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
  await p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await p.send("Page.navigate", { url });
  const h = appHelpers(p, outDir, { desktop: true });
  await h.waitFor("worker ready", () => p.eval(`document.getElementById("startMsg").textContent !== "準備中…"`), 20_000);
  const setFiles = async (selector, paths) => {
    const { root: doc } = await p.send("DOM.getDocument");
    const { nodeId } = await p.send("DOM.querySelector", { nodeId: doc.nodeId, selector });
    await p.send("DOM.setFileInputFiles", { nodeId, files: paths });
  };
  const text = (id) => p.eval(`document.getElementById(${JSON.stringify(id)}).textContent`);

  // 1. スナップショットを読み込む
  await setFiles("#snapFile", [resolve(snap)]);
  await h.waitFor("snapshot loaded", async () => /起動: 読み込んだ/.test((await h.status()).log), 120_000);
  await sleep(3000);
  check(true, "スナップショットを読み込んだ");

  // 2. ネットワークをオンにする
  await h.click("menuBtn");
  await sleep(500);
  const cfg = await p.eval(`[document.getElementById("netUrl").value, document.getElementById("netToken").value, location.hash]`);
  check(cfg[0] === `ws://${RELAY}/relay` && cfg[1] === TOKEN && cfg[2] === "", `既定の中継 URL と #relay-token= のトークン: ${cfg[0]}`);
  await p.eval(`(() => {
    const on = document.getElementById("netOn");
    on.checked = true;
    on.dispatchEvent(new Event("change"));
  })()`);
  await h.waitFor("relay ready", async () => /つながっています/.test(await text("netState")), 30_000);
  check(true, `中継につながった: ${await text("netState")}`);
  await h.shot("net-on");

  // 3. 記録しながら IE で example.com を開く
  await h.click("rec");
  await h.waitFor("recording", () => h.visible("recBadge"), 60_000);
  await h.click("menuBtn");
  await sleep(3000);
  await h.tap(12, 8); // スタート
  await sleep(2500);
  await h.tap(60, 51); // Internet Explorer
  await sleep(8000);
  await h.tap(110, 35); // アドレスバー
  await sleep(2000);
  await h.tap(10, 249); // 画面のキーボードを英数に
  await sleep(1500);
  for (const c of "example.com") {
    if (c === ".") await h.key("Period", ".", 190);
    else await h.key("Key" + c.toUpperCase(), c, c.toUpperCase().charCodeAt(0));
  }
  await sleep(1000);
  await h.key("Enter", "Enter", 13);
  await h.waitFor("relay connected example.com", () => /example\.com:80 connected/.test(relayLog), 60_000);
  check(true, "中継が example.com:80 につないだ");
  await sleep(12000);
  await h.shot("example-com");

  // HTTPS: http://10.0.2.2/ から CA を入れ、確認用のリンク（https://example.com/）を開く
  await h.tap(110, 35);
  await sleep(1500);
  for (const c of "10.0.2.2") {
    if (c === ".") await h.key("Period", ".", 190);
    else await h.key("Digit" + c, c, c.charCodeAt(0));
  }
  await h.key("Enter", "Enter", 13);
  await sleep(6000);
  await h.tap(40, 97); // cerulean-ca.cer
  await sleep(6000);
  await h.key("Tab", "Tab", 9); // ダウンロードの「はい」へ
  await sleep(800);
  await h.key("Enter", "Enter", 13);
  await sleep(6000);
  await h.shot("ca-install");
  await h.key("Enter", "Enter", 13); // 証明書のインストールの「はい」
  await sleep(5000);
  await h.shot("ca-installed");
  await h.tap(140, 212); // https://example.com/
  await sleep(3000);
  await h.shot("https-tapped");
  await h.waitFor("relay connected example.com:443 (tls)", () => /example\.com:443 \(tls\) connected/.test(relayLog), 60_000);
  check(true, "中継が example.com:443 に TLS でつないだ");
  await sleep(12000);
  await h.shot("https-example-com");

  // ネットワークがオンのままストレージカードを挿す → フォルダ共有の方式になり、File Explorer の
  // Storage Card にファイルが見える
  const { writeFileSync } = await import("node:fs");
  const src = resolve(outDir, "hello-share.txt");
  writeFileSync(src, "hello from the shared folder\r\n");
  await h.click("menuBtn");
  await sleep(500);
  await p.eval(`document.getElementById("cardSize").value = "16"`);
  await h.click("cardNew");
  await h.waitFor("card created", async () => /抜いています/.test(await text("cardState")), 30_000);
  await setFiles("#cardAddFiles", [src]);
  await h.waitFor("file listed", async () => /hello-share/.test(await text("cardList")), 30_000);
  await h.click("cardInsert");
  await h.waitFor("inserted as share", async () => /フォルダ共有/.test(await text("cardState")), 30_000);
  check(true, `ネットワークと同時にカードを挿した: ${await text("cardState")}`);
  await h.click("menuBtn");
  await sleep(4000);
  await h.tap(12, 8); // スタート
  await sleep(2000);
  await h.tap(50, 171); // プログラム
  await sleep(4000);
  await h.tap(36, 190); // ファイル エクスプローラ
  await sleep(6000);
  await h.tap(43, 306); // 上へ
  await sleep(3000);
  await h.tap(43, 306); // 上へ（マイ デバイス）
  await sleep(3000);
  await h.tap(60, 147); // Storage Card
  await sleep(5000);
  await h.shot("share-storage-card");
  await h.click("menuBtn");
  await sleep(500);
  await h.click("cardEject");
  await h.waitFor("ejected", async () => /抜いています/.test(await text("cardState")), 30_000);
  check(/hello-share/.test(await text("cardList")), "抜いた後もファイルが残っている");

  await h.click("rec");
  await h.waitFor("recording stopped", () => h.visible("recText"), 60_000);
  const script = await text("recText");
  const rx = (script.match(/^@\d+i net rx /gm) ?? []).length;
  check(rx > 5, `記録に受け取ったフレームが入った（${rx} 個）`);
  check(/\d+i share insert cerulean-card-[0-9a-f]+\.img/.test(script) && /\d+i share eject/.test(script), "記録にフォルダ共有の挿抜が入った");
  await h.click("recScript");
  await h.click("recSnap");
  await h.click("recCards");
  await h.waitFor("downloads", () => {
    const f = readdirSync(dlDir);
    return f.some((n) => n.endsWith(".txt")) && f.some((n) => n.endsWith(".snap.gz")) && f.some((n) => n.endsWith(".img"));
  }, 60_000);
  await sleep(2000);

  // 4. CLI で（ネットワークなしで）再生
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
  for (const k of ["cpu_sha256", "ram_sha256", "screen_sha256"]) check(last[k] === want[k], `CLI の再生で ${k} が一致`);
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  if (!headed) chrome?.proc.kill();
  relay.kill();
}
console.log(failed ? "app-net: FAILED" : "app-net: ok");
process.exit(failed ? 1 : 0);
