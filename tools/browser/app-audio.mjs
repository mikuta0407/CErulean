// app-audio.mjs: ブラウザ版（web/www/app）の音の確認。メニューの「音を出す」を
// オンにしてイメージから起動し、起動の早送りをすぐやめて等速で進め、起動音（約 22 億命令目
// = 仮想 16.4 秒）が AudioContext に渡されることを確かめる。
//
//   tools/web-build.sh && tools/serve-bench.py &
//   node tools/browser/app-audio.mjs <イメージ> <出力ディレクトリ> [--headed]
//
// 渡した音は AudioBufferSourceNode.start を横取りして数える（本数・長さ・最大振幅）。
// 自動再生の制限は Chrome の起動オプションで外す（実際の操作では画面に触れたときに始まる）。
import { mkdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { appHelpers, echoConsole, launchChrome, openPage, sleep } from "./cdp.mjs";

const [image, outDir] = process.argv.slice(2).filter((a) => !a.startsWith("--"));
const headed = process.argv.includes("--headed");
const url = process.env.APP_URL ?? "http://localhost:8000/web/www/app/";
if (!image || !outDir) {
  console.error("usage: app-audio.mjs <image> <outdir> [--headed]");
  process.exit(2);
}
mkdirSync(outDir, { recursive: true });
const profile = resolve(outDir, "profile");
rmSync(profile, { recursive: true, force: true });

const SOUND_STEPS = 2_300_000_000; // 起動音（2,215,6xx,xxx〜）が鳴り終わった後

const { proc, port } = await launchChrome({
  chrome: process.env.CHROME ?? "google-chrome",
  profile,
  headless: !headed,
  args: ["--autoplay-policy=no-user-gesture-required"],
});
let failed = false;
try {
  const p = await openPage(port);
  echoConsole(p);
  await p.send("Runtime.enable");
  await p.send("Page.enable");
  await p.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
  await p.send("Page.addScriptToEvaluateOnNewDocument", {
    source: `
      localStorage.setItem("cerulean-audio", JSON.stringify({ on: true, vol: 0.5 }));
      window.__audio = { n: 0, sec: 0, peak: 0 };
      const start = AudioBufferSourceNode.prototype.start;
      AudioBufferSourceNode.prototype.start = function (...a) {
        const b = this.buffer;
        window.__audio.n++;
        window.__audio.sec += b.duration;
        for (let c = 0; c < b.numberOfChannels; c++)
          for (const x of b.getChannelData(c)) window.__audio.peak = Math.max(window.__audio.peak, Math.abs(x));
        return start.apply(this, a);
      };`,
  });
  await p.send("Page.navigate", { url });
  await sleep(1500);
  const h = appHelpers(p, outDir, { desktop: true });
  if (!(await p.eval(`document.getElementById("audioOn").checked`))) throw new Error("audio setting was not restored");

  const { root } = await p.send("DOM.getDocument");
  const { nodeId } = await p.send("DOM.querySelector", { nodeId: root.nodeId, selector: "#imageFile" });
  await p.send("DOM.setFileInputFiles", { nodeId, files: [resolve(image)] });
  await h.waitFor("booted", async () => !(await h.visible("start")), 60_000);
  // 等速にする（早送りの間は音を捨てる）。AudioContext は操作の中で作るので、
  // 画面の外（ヘッダー）をクリックする。
  await h.click("skipTurbo");
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 40, y: 10, button: "left", clickCount: 1 });
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 40, y: 10, button: "left", clickCount: 1 });

  const t0 = Date.now();
  await h.waitFor(
    "startup sound",
    async () => {
      const s = await h.status();
      const a = await p.eval("window.__audio");
      console.log(`${((Date.now() - t0) / 1000).toFixed(0)}s steps=${s.steps} audio=${JSON.stringify(a)}`);
      await sleep(2000);
      return s.steps >= SOUND_STEPS;
    },
    300_000,
  );
  const a = await p.eval("window.__audio");
  const state = await p.eval("new AudioContext().state");
  console.log(`audio: ${a.n} buffers, ${a.sec.toFixed(3)}s, peak ${a.peak.toFixed(3)} (autoplay test context: ${state})`);
  // 起動音は約 0.37 秒（CLI の --audio-out で 16352 フレーム）。等速で追いつけない区間の
  // 取りこぼしはないはず（Worker は取り出した分を全部送る）ので、ほぼ全部が届く。
  if (a.n === 0 || a.sec < 0.3 || a.peak < 0.05) throw new Error("startup sound was not played");
  await h.shot("after-sound");
  console.log("ok");
} catch (e) {
  failed = true;
  console.error("FAILED:", e.message ?? e);
} finally {
  proc.kill();
}
process.exit(failed ? 1 : 0);
