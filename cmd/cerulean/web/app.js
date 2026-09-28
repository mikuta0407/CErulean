// CErulean serve のブラウザ側。画面のロングポーリング描画と、マウス・
// キーボード入力の送信（サーバーが命令境界で適用・記録する）。
"use strict";

const canvas = document.getElementById("screen");
const ctx = canvas.getContext("2d");
const $ = (id) => document.getElementById(id);

async function post(path, body) {
  const r = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body ?? {}),
  });
  const v = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(v.error || r.statusText);
  return v;
}

function log(msg) {
  const el = $("log");
  el.textContent = msg + "\n" + el.textContent;
}

// ---- 画面 ----
// GET /frame?since=seq: 変化があれば RGBA の生データ、なければ 204。
let seq = 0;
async function frameLoop() {
  for (;;) {
    try {
      const r = await fetch(`/frame?since=${seq}`, { cache: "no-store" });
      if (r.status === 200) {
        const w = +r.headers.get("X-Frame-Width");
        const h = +r.headers.get("X-Frame-Height");
        seq = +r.headers.get("X-Frame-Seq");
        const buf = new Uint8ClampedArray(await r.arrayBuffer());
        if (canvas.width !== w || canvas.height !== h) {
          canvas.width = w;
          canvas.height = h;
          applyZoom();
        }
        ctx.putImageData(new ImageData(buf, w, h), 0, 0);
      } else if (r.status !== 204) {
        throw new Error(r.statusText);
      }
    } catch (e) {
      await new Promise((ok) => setTimeout(ok, 1000)); // サーバー停止中など
    }
  }
}

function applyZoom() {
  const z = +$("zoom").value;
  canvas.style.width = canvas.width * z + "px";
  canvas.style.height = canvas.height * z + "px";
}
$("zoom").addEventListener("change", applyZoom);

// ---- 入力の送信 ----
// 送信は 1 本の列に並べて順序を保つ（down の前に move が届く等を防ぐ）。
let queue = Promise.resolve();
function send(ev) {
  queue = queue.then(() => post("/input", ev)).catch((e) => log("入力エラー: " + e.message));
}

// 表示上の座標 → 画面のピクセル座標（拡大縮小込み、範囲内に丸める）。
function toScreen(e) {
  const r = canvas.getBoundingClientRect();
  const x = Math.floor(((e.clientX - r.left) * canvas.width) / r.width);
  const y = Math.floor(((e.clientY - r.top) * canvas.height) / r.height);
  return {
    x: Math.min(Math.max(x, 0), canvas.width - 1),
    y: Math.min(Math.max(y, 0), canvas.height - 1),
  };
}

let penDown = false;
let pendingMove = null; // move は描画フレームごとに最新の 1 個だけ送る
canvas.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  canvas.focus();
  canvas.setPointerCapture(e.pointerId);
  penDown = true;
  send({ t: "down", ...toScreen(e) });
  e.preventDefault();
});
canvas.addEventListener("pointermove", (e) => {
  if (!penDown) return;
  if (!pendingMove) {
    requestAnimationFrame(() => {
      if (penDown && pendingMove) send({ t: "move", ...pendingMove });
      pendingMove = null;
    });
  }
  pendingMove = toScreen(e);
});
function penUp() {
  if (!penDown) return;
  penDown = false;
  pendingMove = null;
  send({ t: "up" });
}
canvas.addEventListener("pointerup", penUp);
canvas.addEventListener("pointercancel", penUp);

// PC のキー（KeyboardEvent.code。配列に依存しない物理位置）→ machine のキー名。
const keyMap = {
  ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
  Enter: "Enter", NumpadEnter: "Enter",
  F1: "App1", F2: "App2", F3: "App3", F4: "App4", F5: "App5",
  Space: "Space", Backspace: "Back", Tab: "Tab", Escape: "Esc", Delete: "Delete",
  ShiftLeft: "Shift", ShiftRight: "RShift", ControlLeft: "Ctrl", ControlRight: "Ctrl",
  AltLeft: "Alt", AltRight: "Alt", CapsLock: "CapsLock",
};
for (const c of "ABCDEFGHIJKLMNOPQRSTUVWXYZ") keyMap["Key" + c] = c;
for (const d of "0123456789") keyMap["Digit" + d] = d;

const held = new Set();
canvas.addEventListener("keydown", (e) => {
  const k = keyMap[e.code];
  if (!k) return;
  e.preventDefault();
  if (e.repeat || held.has(e.code)) return; // 自動リピートは OS 側に任せる
  held.add(e.code);
  send({ t: "keydown", key: k });
});
canvas.addEventListener("keyup", (e) => {
  const k = keyMap[e.code];
  if (!k) return;
  e.preventDefault();
  if (held.delete(e.code)) send({ t: "keyup", key: k });
});
// フォーカスを失ったら押しっぱなしを解除する（keyup が届かなくなるため）。
function releaseAll() {
  for (const code of held) send({ t: "keyup", key: keyMap[code] });
  held.clear();
  penUp();
}
canvas.addEventListener("blur", releaseAll);
window.addEventListener("blur", releaseAll);

// ---- 操作パネル ----
let paused = false;
$("pause").addEventListener("click", async () => {
  try {
    await post("/control", { op: paused ? "resume" : "pause" });
  } catch (e) {
    log(e.message);
  }
});
$("speed").addEventListener("change", async () => {
  try {
    await post("/control", { op: "speed", speed: +$("speed").value });
  } catch (e) {
    log(e.message);
  }
});
$("snap").addEventListener("click", async () => {
  try {
    const v = await post("/snapshot");
    log(`保存: ${v.path}（命令 ${v.step}）`);
  } catch (e) {
    log("保存エラー: " + e.message);
  }
});
let recording = false;
$("rec").addEventListener("click", async () => {
  try {
    if (!recording) {
      const v = await post("/record/start");
      log(`記録開始: 起点 ${v.snapshot}（命令 ${v.step}）`);
    } else {
      const v = await post("/record/stop");
      log(`記録停止: ${v.events} 件 → ${v.script}\n期待画面: ${v.expected}\n再生: ${v.command}\n\n${v.text}`);
    }
  } catch (e) {
    log("記録エラー: " + e.message);
  }
});

function fmtTime(sec) {
  const m = Math.floor(sec / 60);
  return `${m}:${(sec - m * 60).toFixed(1).padStart(4, "0")}`;
}
async function statusLoop() {
  for (;;) {
    try {
      const s = await (await fetch("/status", { cache: "no-store" })).json();
      $("steps").textContent = s.steps.toLocaleString();
      $("vtime").textContent = fmtTime(s.virtualSec);
      $("ratio").textContent = s.paused ? "停止中" : `${s.ratio.toFixed(2)} 倍`;
      $("mips").textContent = `${s.mips.toFixed(1)}M 命令/秒`;
      $("idle").textContent = `${(s.idleSkipped * 100).toFixed(1)}% をスキップ`;
      $("err").textContent = s.error ? "停止: " + s.error : "";
      paused = s.paused;
      $("pause").textContent = paused ? "再開" : "一時停止";
      $("pause").classList.toggle("on", paused);
      recording = s.recording;
      $("rec").textContent = recording ? "記録停止" : "記録開始";
      $("rec").classList.toggle("on", recording);
      const sp = String(s.speed);
      if ($("speed").value !== sp && document.activeElement !== $("speed")) $("speed").value = sp;
    } catch (e) {
      $("err").textContent = "サーバーに接続できません";
    }
    await new Promise((ok) => setTimeout(ok, 500));
  }
}

applyZoom();
frameLoop();
statusLoop();
