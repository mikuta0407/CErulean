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
