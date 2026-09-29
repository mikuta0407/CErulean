// ブラウザ版の UI（メインスレッド）。エミュレーションは worker.js が行い、ここは
// 画面の描画・入力（タッチ・ハードウェアボタン・PC のキー）・操作パネルだけを持つ
// （計画書 §3.4・§7.1）。入力の対応表と座標変換は Go 版の serve の UI
// （www/legacy-serve/app.js）から移したもの。
const $ = (id) => document.getElementById(id);
const canvas = $("screen");
const ctx = canvas.getContext("2d");

const worker = new Worker("worker.js", { type: "module" });
const send = (op, args = {}, transfer = []) => worker.postMessage({ op, ...args }, transfer);

function log(msg) {
  const el = $("log");
  el.textContent = (msg + "\n" + el.textContent).slice(0, 20000);
}

let booted = false;
let stopped = false;
let paused = false;
let images = [];

// ---- Worker からのメッセージ ----
worker.onmessage = ({ data: d }) => {
  if (d.log) log(d.log);
  if (d.images) {
    images = d.images;
    renderImages();
  }
  if (d.booted) {
    booted = true;
    stopped = false;
    $("start").hidden = true;
    $("stopBanner").hidden = true;
    $("pause").disabled = false;
    log(`起動: ${d.booted.name}${d.booted.imageId ? `（${d.booted.imageId.slice(0, 12)}…）` : ""}`);
  }
  if (d.frame) drawFrame(d.frame, d.w, d.h);
  if (d.status) showStatus(d.status);
  if (d.stopped) showStop(`エミュレーションが止まりました。\n${d.stopped}`);
  if (d.error) {
    log("エラー: " + d.error);
    if (booted) showStop(d.error);
    else alert(d.error);
  }
};
worker.onerror = (e) => {
  log(`Worker の異常終了: ${e.message}`);
  showStop(`Worker が異常終了しました: ${e.message}`);
};
send("init");

function showStop(text) {
  stopped = true;
  $("stopText").textContent = text;
  $("stopBanner").hidden = false;
}

// ---- 画面 ----
function drawFrame(buf, w, h) {
  if (!w || !h) {
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    return;
  }
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
    fitScreen();
  }
  ctx.putImageData(new ImageData(new Uint8ClampedArray(buf), w, h), 0, 0);
}

// 画面の枠に収まる最大の大きさに合わせる。デバイスピクセルの整数倍で 85% 以上の
// 大きさにできるときは整数倍にする（ドットの幅がそろう。§7.1）。
function fitScreen() {
  const box = $("screenBox").getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const margin = 12; // 枠線の分
  const fit = Math.min((box.width - margin) / canvas.width, (box.height - margin) / canvas.height);
  let scale = Math.floor(fit * dpr) / dpr;
  if (scale < fit * 0.85) scale = fit;
  canvas.style.width = `${canvas.width * scale}px`;
  canvas.style.height = `${canvas.height * scale}px`;
}
new ResizeObserver(fitScreen).observe($("screenBox"));
window.addEventListener("resize", fitScreen);

// ---- 状態表示 ----
function fmtTime(sec) {
  const m = Math.floor(sec / 60);
  return `${m}:${(sec - m * 60).toFixed(1).padStart(4, "0")}`;
}
function showStatus(s) {
  paused = s.paused;
  $("pause").classList.toggle("on", paused);
  $("pause").textContent = paused ? "▶" : "❚❚";
  $("pause").title = paused ? "再開" : "一時停止";
  $("steps").textContent = BigInt(s.steps).toLocaleString();
  $("vtime").textContent = fmtTime(s.virtualSec);
  $("ratio").textContent = paused ? "一時停止中" : `${s.ratio.toFixed(2)} 倍`;
  $("mips").textContent = `${s.mips.toFixed(1)}M 命令/秒`;
  $("idle").textContent = `${(s.idle * 100).toFixed(1)}% をスキップ`;
  const badge = $("badge");
  badge.hidden = !(s.turbo && !paused) && !s.jitError;
  badge.classList.toggle("warn", !!s.jitError);
  badge.textContent = s.jitError ? "JIT 停止" : "起動中（早送り）";
  badge.title = s.jitError ?? "";
  $("skipTurbo").hidden = !(s.turbo && s.speed !== 0);
  $("statusLine").textContent = paused
    ? "一時停止中"
    : `${fmtTime(s.virtualSec)}  ×${s.ratio.toFixed(2)}  ${s.mips.toFixed(0)}M/s`;
  if (s.uart) {
    const el = $("uart");
    el.textContent = (el.textContent + s.uart).slice(-50000);
  }
}

// ---- 起動 ----
// ゲストの RTC はホストのローカル時刻から（起動時だけ。以後は命令数で進む）。
function rtcNow() {
  const d = new Date();
  return [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes(), d.getSeconds()];
}

$("imageFile").addEventListener("change", async (e) => {
  const f = e.target.files[0];
  e.target.value = "";
  if (!f) return;
  const bytes = new Uint8Array(await f.arrayBuffer());
  send("bootFile", { bytes, name: f.name, rtc: rtcNow(), jit: $("jit").checked }, [bytes.buffer]);
});

function renderImages() {
  const ul = $("imageList");
  ul.replaceChildren();
  for (const im of images) {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.className = "boot";
    b.innerHTML = `<span></span><small></small>`;
    b.firstChild.textContent = `${im.name} で起動`;
    b.lastChild.textContent = `${(im.size / 1e6).toFixed(1)}MB・${im.id.slice(0, 12)}…`;
    b.onclick = () => send("bootStored", { id: im.id, rtc: rtcNow(), jit: $("jit").checked });
    const del = document.createElement("button");
    del.className = "del";
    del.textContent = "✕";
    del.title = "この端末から消す";
    del.onclick = () => confirm(`${im.name} をこの端末から消しますか？`) && send("deleteImage", { id: im.id });
    li.append(b, del);
    ul.append(li);
  }
}

$("reboot").onclick = () => {
  $("stopBanner").hidden = true;
  $("start").hidden = false;
};
$("changeImage").onclick = () => {
  send("pause", { on: true });
  $("menu").hidden = true;
  $("start").hidden = false;
};

// ---- 操作パネル ----
$("menuBtn").onclick = () => ($("menu").hidden = !$("menu").hidden);
$("pause").onclick = () => send("pause", { on: !paused });
$("speed").onchange = () => send("speed", { v: +$("speed").value });
$("jit").onchange = () => send("jit", { on: $("jit").checked });
$("skipTurbo").onclick = () => send("skipTurbo");
// 非表示中は止める（§7.4）
document.addEventListener("visibilitychange", () => send("visibility", { hidden: document.hidden }));

// ---- 入力 ----
const input = (t, args = {}) => {
  if (booted && !stopped) send("input", { t, ...args });
};

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

// タッチ（1 本の指・マウス・ペン）。2 本目の指は無視する（ゲストは 1 点のタッチパネル）。
let penId = null;
let pendingMove = null; // move は描画フレームごとに最新の 1 個だけ送る
canvas.addEventListener("pointerdown", (e) => {
  if (penId !== null || (e.pointerType === "mouse" && e.button !== 0)) return;
  penId = e.pointerId;
  canvas.setPointerCapture(e.pointerId);
  input("down", toScreen(e));
  e.preventDefault();
});
canvas.addEventListener("pointermove", (e) => {
  if (e.pointerId !== penId) return;
  if (!pendingMove) {
    requestAnimationFrame(() => {
      if (penId !== null && pendingMove) input("move", pendingMove);
      pendingMove = null;
    });
  }
  pendingMove = toScreen(e);
});
function penUp(e) {
  if (e && e.pointerId !== penId) return;
  if (penId === null) return;
  penId = null;
  pendingMove = null;
  input("up");
}
canvas.addEventListener("pointerup", penUp);
canvas.addEventListener("pointercancel", penUp);
canvas.addEventListener("contextmenu", (e) => e.preventDefault());

// 押しているキー（ゲストのキー名 → 押している元の集合）。画面外のボタンと PC のキーが
// 同じキーを押していても、両方離すまで離したことにしない。
const held = new Map();
function press(key, src) {
  let s = held.get(key);
  if (!s) held.set(key, (s = new Set()));
  if (s.size === 0) input("keydown", { key });
  s.add(src);
}
function release(key, src) {
  const s = held.get(key);
  if (!s || !s.delete(src)) return;
  if (s.size === 0) input("keyup", { key });
}

// 画面外のハードウェアボタン。
for (const b of document.querySelectorAll(".hw")) {
  const key = b.dataset.key;
  const up = (e) => {
    b.classList.remove("down-now");
    release(key, "btn" + e.pointerId);
  };
  b.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    b.setPointerCapture(e.pointerId);
    b.classList.add("down-now");
    press(key, "btn" + e.pointerId);
    navigator.vibrate?.(8);
  });
  b.addEventListener("pointerup", up);
  b.addEventListener("pointercancel", up);
  b.addEventListener("lostpointercapture", up);
  b.addEventListener("contextmenu", (e) => e.preventDefault());
}

// PC のキー（KeyboardEvent.code。配列に依存しない物理位置）→ ゲストのキー名。
const keyMap = {
  ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
  Enter: "Enter", NumpadEnter: "Enter",
  F1: "App1", F2: "App2", F3: "App3", F4: "App4", F5: "App5",
  Space: "Space", Backspace: "Back", Tab: "Tab", Escape: "Esc", Delete: "Delete",
  ShiftLeft: "Shift", ShiftRight: "RShift", ControlLeft: "Ctrl", ControlRight: "Ctrl",
  AltLeft: "Alt", AltRight: "Alt", CapsLock: "CapsLock",
};
for (const c of "ABCDEFGHIJKLMNOPQRSTUVWXYZ") keyMap["Key" + c] = c;
for (const d of "0123456789") {
  keyMap["Digit" + d] = d;
  keyMap["Numpad" + d] = d;
}

// メニューの入力欄にフォーカスがあるときは、キーをゲストに送らない。
const typingInUi = (e) => e.target instanceof HTMLElement && e.target.closest("#menu, .overlay:not([hidden])");
window.addEventListener("keydown", (e) => {
  // IME の変換中のキーは送らない（§7.1。keyCode 229 は変換中を示す古い値）
  if (e.isComposing || e.keyCode === 229 || typingInUi(e) || !booted) return;
  const k = keyMap[e.code];
  if (!k || e.metaKey) return;
  e.preventDefault();
  if (e.repeat) return; // 自動リピートはゲスト側に任せる
  press(k, "kbd:" + e.code);
});
window.addEventListener("keyup", (e) => {
  const k = keyMap[e.code];
  if (!k) return;
  e.preventDefault();
  release(k, "kbd:" + e.code);
});
// フォーカスを失ったら押しっぱなしを解除する（keyup が届かなくなるため）。
window.addEventListener("blur", () => {
  for (const [key, s] of held) {
    for (const src of [...s]) if (src.startsWith("kbd:")) release(key, src);
  }
  penUp();
});
