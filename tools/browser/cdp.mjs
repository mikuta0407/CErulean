// cdp.mjs: Chrome DevTools Protocol の最小のクライアント（依存なし。Node 24 の
// WebSocket を使う）。ブラウザ版の動作確認（tools/browser/app-smoke.mjs）用。
import { spawn } from "node:child_process";

export async function launchChrome({ chrome = "google-chrome", port = 9333, profile, headless = true, args = [] }) {
  const proc = spawn(
    chrome,
    [
      ...(headless ? ["--headless=new"] : []),
      `--remote-debugging-port=${port}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      "--no-default-browser-check",
      ...args,
      "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  proc.stderr.on("data", () => {});
  for (let i = 0; i < 100; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (r.ok) return { proc, port };
    } catch {}
    await new Promise((ok) => setTimeout(ok, 100));
  }
  proc.kill();
  throw new Error("chrome did not start");
}

export async function openPage(port) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const page = list.find((t) => t.type === "page");
  return connect(page.webSocketDebuggerUrl);
}

export function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let id = 0;
    const waiting = new Map();
    const listeners = [];
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && waiting.has(m.id)) {
        const { ok, ng } = waiting.get(m.id);
        waiting.delete(m.id);
        if (m.error) ng(new Error(`${m.error.message} ${m.error.data ?? ""}`));
        else ok(m.result);
      } else if (m.method) {
        for (const f of listeners) f(m);
      }
    };
    ws.onerror = reject;
    ws.onopen = () =>
      resolve({
        send(method, params = {}) {
          return new Promise((ok, ng) => {
            waiting.set(++id, { ok, ng });
            ws.send(JSON.stringify({ id, method, params }));
          });
        },
        on(f) {
          listeners.push(f);
        },
        async eval(expr) {
          const r = await this.send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
          if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
          return r.result.value;
        },
        close() {
          ws.close();
        },
      });
  });
}

export const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));

// ブラウザ版（rust/web/www/app）の操作の道具。desktop ならマウス、でなければタッチで操作する。
let shotN = 0; // 撮った順の番号（ブラウザを開き直しても続ける）
export function appHelpers(p, outDir, { desktop = false } = {}) {
  const h = {
    async shot(name) {
      const r = await p.send("Page.captureScreenshot", { format: "png" });
      const { writeFileSync } = await import("node:fs");
      const f = `${outDir}/${String(++shotN).padStart(2, "0")}-${name}.png`;
      writeFileSync(f, Buffer.from(r.data, "base64"));
      console.log("shot", f);
    },
    status: () =>
      p.eval(`({steps: Number(document.getElementById("steps").textContent.replace(/,/g, "")) || 0,
        line: document.getElementById("statusLine").textContent,
        log: document.getElementById("log").textContent})`),
    visible: (id) => p.eval(`!!document.getElementById(${JSON.stringify(id)}) && !document.getElementById(${JSON.stringify(id)}).hidden`),
    click: (id) => p.eval(`document.getElementById(${JSON.stringify(id)}).click()`),
    async waitFor(desc, pred, timeoutMs = 300_000) {
      const t0 = Date.now();
      for (;;) {
        const v = await pred();
        if (v) return v;
        if (Date.now() - t0 > timeoutMs) throw new Error(`timeout: ${desc}`);
        await sleep(500);
      }
    },
    async screenRect() {
      return p.eval(`(() => { const r = document.getElementById("screen").getBoundingClientRect(); return {x: r.left, y: r.top, w: r.width, h: r.height}; })()`);
    },
    // 画面座標（240×320）をタップする
    async tap(gx, gy) {
      const r = await h.screenRect();
      const pt = { x: r.x + ((gx + 0.5) * r.w) / 240, y: r.y + ((gy + 0.5) * r.h) / 320 };
      if (desktop) {
        await p.send("Input.dispatchMouseEvent", { type: "mousePressed", ...pt, button: "left", clickCount: 1 });
        await sleep(150);
        await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...pt, button: "left", clickCount: 1 });
      } else {
        await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [pt] });
        await sleep(150);
        await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
      }
    },
    // 画面外のハードウェアボタンを押す
    async hw(key) {
      const r = await p.eval(`(() => { const r = document.querySelector('[data-key="${key}"]').getBoundingClientRect(); return {x: r.left + r.width / 2, y: r.top + r.height / 2}; })()`);
      if (desktop) {
        await p.send("Input.dispatchMouseEvent", { type: "mousePressed", ...r, button: "left", clickCount: 1 });
        await sleep(120);
        await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...r, button: "left", clickCount: 1 });
      } else {
        await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [r] });
        await sleep(120);
        await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
      }
    },
    // PC のキーを押して離す
    async key(code, key, keyCode) {
      await p.send("Input.dispatchKeyEvent", { type: "rawKeyDown", code, key, windowsVirtualKeyCode: keyCode });
      await sleep(80);
      await p.send("Input.dispatchKeyEvent", { type: "keyUp", code, key, windowsVirtualKeyCode: keyCode });
      await sleep(80);
    },
  };
  return h;
}

// ページのコンソール・例外を表示する
export function echoConsole(p, prefix = "") {
  p.on((m) => {
    if (m.method === "Runtime.consoleAPICalled") console.log(prefix + "console:", m.params.args.map((a) => a.value ?? a.description).join(" "));
    if (m.method === "Runtime.exceptionThrown") console.log(prefix + "exception:", JSON.stringify(m.params.exceptionDetails).slice(0, 500));
  });
}
