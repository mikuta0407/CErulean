// Web 資材入りのバイナリをリポジトリの外へコピーし、埋め込み配信と静的配信を確認する。
// node tools/browser/app-distribution.mjs <cerulean-binary>
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, statSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { createServer as createTcpServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve, extname } from "node:path";
import { appHelpers, launchChrome, openPage, sleep } from "./cdp.mjs";

const input = process.argv[2];
if (!input) throw new Error("usage: app-distribution.mjs <cerulean-binary>");
const dir = mkdtempSync(join(tmpdir(), "cerulean-distribution-"));
const binary = join(dir, "cerulean");
copyFileSync(resolve(input), binary);
const site = join(dir, "site");
execFileSync(binary, ["web-export", site], { cwd: dir });
const pkgAssets = JSON.parse(readFileSync(join(site, "pkg/assets.json")));
assert(pkgAssets.some((name) => name.startsWith("snippets/")));
for (const name of pkgAssets) assert(statSync(join(site, "pkg", name)).isFile());

async function freePort() {
  const server = createTcpServer();
  await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
  const port = server.address().port;
  await new Promise((ok) => server.close(ok));
  return port;
}
const port = await freePort();
const serve = spawn(binary, ["serve", "--port", String(port)], { cwd: dir, stdio: ["ignore", "ignore", "pipe"] });
let log = "";
serve.stderr.on("data", (b) => { log += b; });
// Serve the export under a subdirectory to check relative Worker / wasm / PWA URLs too.
const mime = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm", ".json": "application/json", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".webmanifest": "application/manifest+json" };
const staticServer = createServer((req, res) => {
  const url = new URL(req.url, "http://localhost");
  if (!url.pathname.startsWith("/demo/")) { res.writeHead(404).end(); return; }
  let path = join(site, url.pathname.slice("/demo/".length));
  try {
    if (statSync(path).isDirectory()) path = join(path, "index.html");
    res.writeHead(200, { "Content-Type": mime[extname(path)] ?? "application/octet-stream" });
    res.end(readFileSync(path));
  } catch { res.writeHead(404).end(); }
});
let chrome = null;
let page = null;
try {
  await new Promise((ok) => staticServer.listen(0, "127.0.0.1", ok));
  for (let i = 0; i < 100 && !log.includes("open http:"); i++) await sleep(100);
  assert(log.includes("embedded Web assets"), log);
  const root = await fetch(`http://127.0.0.1:${port}/`, { redirect: "manual" });
  assert.equal(root.status, 302);
  assert.equal(root.headers.get("location"), "/app/");
  const wasm = await fetch(`http://127.0.0.1:${port}/pkg/cerulean_web_bg.wasm`);
  assert.equal(wasm.headers.get("content-type"), "application/wasm");
  assert.deepEqual(Buffer.from(await wasm.arrayBuffer()), readFileSync(join(site, "pkg/cerulean_web_bg.wasm")));
  assert.equal((await fetch(`http://127.0.0.1:${port}/relay`)).status, 404);
  chrome = await launchChrome({ profile: join(dir, "chrome"), port: await freePort() });
  page = await openPage(chrome.port);
  await page.send("Runtime.enable");
  await page.send("Page.enable");
  await page.send("Network.enable");
  await page.send("Network.setCacheDisabled", { cacheDisabled: true });
  const helper = appHelpers(page, dir, { desktop: true });
  const staticPort = staticServer.address().port;
  for (const [kind, url] of [
    ["embedded", `http://127.0.0.1:${port}/app/`],
    ["static subdirectory", `http://127.0.0.1:${staticPort}/demo/app/`],
  ]) {
    await page.eval(`if (document.getElementById("startMsg")) document.getElementById("startMsg").textContent = "navigation pending"`);
    await page.send("Page.navigate", { url });
    await helper.waitFor(`${kind}: wasm Worker ready`, () => page.eval(`document.getElementById("startMsg")?.textContent === "イメージを選んでください。"`), 30_000);
    await helper.waitFor(`${kind}: Service Worker ready`, () => page.eval(`!!navigator.serviceWorker.controller`), 30_000);
    const missing = await page.eval(`(async () => {
      const files = await (await fetch("../pkg/assets.json")).json();
      const missing = [];
      for (const name of files) if (!await caches.match(new URL("../pkg/" + name, location.href).href)) missing.push(name);
      return missing;
    })()`);
    assert.deepEqual(missing, []);
    await page.send("Network.emulateNetworkConditions", { offline: true, latency: 0, downloadThroughput: 0, uploadThroughput: 0 });
    await page.eval(`document.getElementById("startMsg").textContent = "reload pending"`);
    await page.send("Page.reload", { ignoreCache: true });
    await helper.waitFor(`${kind}: offline wasm Worker ready`, () => page.eval(`document.getElementById("startMsg")?.textContent === "イメージを選んでください。"`), 30_000);
    // After reload the new Worker can only initialize if the wasm and imported snippets are available.
    assert.equal(await page.eval("navigator.onLine"), false);
    await page.send("Network.emulateNetworkConditions", { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
    console.log(`ok: ${kind}, Worker + wasm + snippets + offline reload`);
  }
} finally {
  page?.close();
  if (chrome) {
    const done = new Promise((ok) => chrome.proc.once("exit", ok));
    chrome.proc.kill();
    await done;
  }
  const done = new Promise((ok) => serve.once("exit", ok));
  serve.kill();
  if (serve.exitCode === null && serve.signalCode === null) await done;
  await new Promise((ok) => staticServer.close(ok));
  rmSync(dir, { recursive: true, force: true });
}
