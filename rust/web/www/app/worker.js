// ブラウザ版のエミュレーション Worker（計画書 §3.4・§7.2）。
//
// wasm のマシンを 1 台持ち、実時間に合わせて小さな単位で進める。1 単位ごとに制御を
// 返し（MessageChannel）、その間にメインからの入力を受け取る。入力は受け取った時点
// （run の合間 = 命令境界）で emu.input に渡す。画面は変化したときだけ RGBA を
// transfer で送る。壁時計（performance.now）を見るのはここだけで、ゲストの時刻は
// 命令数から決まる（決定論性）。
import init, * as wasm from "../pkg/cerulean_web.js";

const IPS = 135_200_000; // 仮想時間 1 秒あたりの命令数（smdk2410 の INSTRUCTIONS_PER_SECOND）
// 1 回の run で進める仮想時間（10ms）。入力の反映の遅れはこれ以下になる。
const SLICE = IPS / 100;
// 1 回の tick で実行に使う壁時計の上限（ms）。これを過ぎたら制御を返す。
const BUDGET_MS = 12;
// これより遅れたら追いつくのを諦める（遅い端末で遅れがたまり続けないように）。
const MAX_LAG_SEC = 0.25;
// イメージから起動したとき、この命令数（Today の完成が約 34〜35 億命令目）までは
// 最高速で進める（計画書 §7.1 の初回起動の体験）。
const BOOT_TURBO_STEPS = 3_600_000_000n;
const FRAME_MS = 33;
const STATUS_MS = 500;
const JIT_PARAMS = [64, 32]; // 計測ページの既定と同じ（閾値・まとめるブロック数）

let emu = null;
let imageId = "";
let speed = 1; // 0 = 最高速
let paused = false;
let hidden = false;
let stopped = false;
let turboUntil = 0n;
// 実時間との対応の基準（この壁時計の時刻に、この命令数だった）
let baseT = 0;
let baseSteps = 0n;
let scheduled = false;
let lastFrame = null;
let lastFrameT = 0;
let st = { t: 0, steps: 0n, idle: 0n };
const uartDec = new TextDecoder("latin1");

const post = (m, transfer) => postMessage(m, transfer ?? []);
const log = (msg) => post({ log: msg });

// ---- ストレージ（OPFS。イメージを SHA-256 をキーに置く。§7.3）----

async function imagesDir() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle("images", { create: true });
}

async function writeFile(dir, name, bytes) {
  // 一時ファイルに書き終えてから名前を変える（書きかけを残さない。§7.3）
  const tmp = await dir.getFileHandle(name + ".tmp", { create: true });
  const h = await tmp.createSyncAccessHandle();
  try {
    h.truncate(0);
    h.write(bytes, { at: 0 });
    h.flush();
  } finally {
    h.close();
  }
  await tmp.move(name);
}

async function listImages() {
  try {
    const dir = await imagesDir();
    const out = [];
    for await (const [name, fh] of dir.entries()) {
      if (!name.endsWith(".json")) continue;
      try {
        out.push(JSON.parse(await (await fh.getFile()).text()));
      } catch {
        // 壊れたメタデータは無視する
      }
    }
    out.sort((a, b) => (b.used ?? 0) - (a.used ?? 0));
    return out;
  } catch (e) {
    log(`OPFS を使えません: ${e.message}`);
    return [];
  }
}

async function storeImage(bytes, name) {
  if (!globalThis.crypto?.subtle) return ""; // 安全なコンテキストでない（http の LAN IP 等）
  const id = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)), (x) =>
    x.toString(16).padStart(2, "0"),
  ).join("");
  try {
    const dir = await imagesDir();
    await writeFile(dir, `${id}.bin`, bytes);
    await writeFile(dir, `${id}.json`, new TextEncoder().encode(JSON.stringify({ id, name, size: bytes.length, used: Date.now() })));
  } catch (e) {
    log(`イメージを保存できません: ${e.message}`);
  }
  return id;
}

async function loadStoredImage(id) {
  const dir = await imagesDir();
  const bytes = new Uint8Array(await (await (await dir.getFileHandle(`${id}.bin`)).getFile()).arrayBuffer());
  const mh = await dir.getFileHandle(`${id}.json`);
  const meta = JSON.parse(await (await mh.getFile()).text());
  meta.used = Date.now();
  await writeFile(dir, `${id}.json`, new TextEncoder().encode(JSON.stringify(meta)));
  return { bytes, name: meta.name };
}

// ---- 実行 ----

function rebase() {
  baseT = performance.now();
  baseSteps = emu.steps();
}

function schedule(delayMs = 0) {
  if (scheduled) return;
  scheduled = true;
  if (delayMs > 0) setTimeout(tick, delayMs);
  else chan.port2.postMessage(0);
}
const chan = new MessageChannel();
chan.port1.onmessage = () => tick();

function running() {
  return emu && !paused && !hidden && !stopped;
}

function runTo(target) {
  const r = emu.run(target);
  if (r === "ok") return true;
  stopped = true;
  post({ stopped: emu.stopJson() });
  return false;
}

function tick() {
  scheduled = false;
  if (!running()) return;
  let delay = 0;
  try {
    const t0 = performance.now();
    const turbo = speed === 0 || emu.steps() < turboUntil;
    if (turbo) {
      const chunk = BigInt(SLICE * 4);
      while (performance.now() - t0 < BUDGET_MS && runTo(emu.steps() + chunk));
      rebase();
    } else {
      const rate = IPS * speed; // 実時間 1 秒あたりの命令数
      const slice = BigInt(Math.round(SLICE * speed));
      let target = baseSteps + BigInt(Math.floor(((t0 - baseT) * rate) / 1000));
      let cur = emu.steps();
      if (Number(target - cur) > rate * MAX_LAG_SEC) {
        rebase(); // 追いつくのを諦める
        target = cur;
      }
      // 期限より 1 単位先まで進めて、その分だけ待つ
      while (cur <= target && performance.now() - t0 < BUDGET_MS) {
        if (!runTo(cur + slice)) break;
        cur = emu.steps();
      }
      if (cur > target) delay = Math.max(1, Math.ceil((Number(cur - target) * 1000) / rate));
    }
    const now = performance.now();
    if (now - lastFrameT >= FRAME_MS) {
      lastFrameT = now;
      sendFrame();
    }
    if (now - st.t >= STATUS_MS) sendStatus(now);
  } catch (e) {
    fatal(e);
    return;
  }
  if (running()) schedule(delay);
}

function sendFrame(force = false) {
  const f = emu.frame();
  const w = emu.frameWidth();
  const h = emu.frameHeight();
  if (!force && lastFrame && sameBytes(lastFrame, f)) return;
  lastFrame = f;
  const copy = f.slice();
  post({ frame: copy.buffer, w, h }, [copy.buffer]);
}

function sameBytes(a, b) {
  if (a.length !== b.length) return false;
  const x = new Uint32Array(a.buffer, a.byteOffset, a.length >> 2);
  const y = new Uint32Array(b.buffer, b.byteOffset, b.length >> 2);
  for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
  return true;
}

function sendStatus(now) {
  const steps = emu.steps();
  const idle = emu.idleSkipped();
  const dt = (now - st.t) / 1000;
  const ds = Number(steps - st.steps);
  const s = {
    steps: steps.toString(),
    virtualSec: Number(steps) / IPS,
    ratio: st.t && dt > 0 ? ds / IPS / dt : 0,
    mips: st.t && dt > 0 ? ds / dt / 1e6 : 0,
    idle: ds > 0 ? Number(idle - st.idle) / ds : 0,
    paused,
    turbo: speed === 0 || steps < turboUntil,
    speed,
  };
  const uart = emu.takeUart();
  if (uart.length) s.uart = uartDec.decode(uart);
  const js = JSON.parse(emu.jitStats());
  if (js.error) s.jitError = js.error;
  s.jitModules = js.modules;
  st = { t: now, steps, idle };
  post({ status: s });
}

// wasm の panic（RuntimeError）はインスタンスを使えなくする（§6.2）。以後は動かさない。
function fatal(e) {
  stopped = true;
  post({ error: `${e?.name ?? "Error"}: ${e?.message ?? e}` });
}

async function boot(bytes, name, rtc, jit) {
  emu?.free();
  emu = new wasm.Emu();
  emu.loadImage(bytes, name, Int32Array.from(rtc));
  if (jit) emu.setJit(true, ...JIT_PARAMS);
  stopped = false;
  paused = false;
  lastFrame = null;
  turboUntil = BOOT_TURBO_STEPS;
  st = { t: 0, steps: 0n, idle: 0n };
  rebase();
  post({ booted: { imageId, name } });
  schedule();
}

const handlers = {
  async init() {
    await init();
    wasm.installPanicHook();
    post({ ready: true, images: await listImages() });
  },
  async bootFile({ bytes, name, rtc, jit }) {
    imageId = await storeImage(bytes, name);
    await boot(bytes, name, rtc, jit);
    post({ images: await listImages() });
  },
  async bootStored({ id, rtc, jit }) {
    const { bytes, name } = await loadStoredImage(id);
    imageId = id;
    await boot(bytes, name, rtc, jit);
  },
  async deleteImage({ id }) {
    const dir = await imagesDir();
    for (const n of [`${id}.bin`, `${id}.json`]) await dir.removeEntry(n).catch(() => {});
    post({ images: await listImages() });
  },
  input({ t, x = 0, y = 0, key = "" }) {
    if (!emu || stopped) return;
    emu.input(t, x, y, key);
    // 入力の直後の画面の変化を早く見せるため、止まっているときも次の描画を促す
    if (!running()) sendFrame();
  },
  pause({ on }) {
    paused = on;
    if (emu && !on) {
      rebase();
      schedule();
    }
    if (emu) sendStatus(performance.now());
  },
  visibility({ hidden: h }) {
    // 非表示中は止める（ブラウザがタイマーを絞るため。決定論的なので結果は変わらない。§7.4）
    hidden = h;
    if (emu && !h) {
      rebase();
      schedule();
    }
  },
  speed({ v }) {
    speed = v;
    if (emu) rebase();
  },
  skipTurbo() {
    turboUntil = 0n;
    if (emu) rebase();
  },
  jit({ on }) {
    if (emu) emu.setJit(on, ...JIT_PARAMS);
  },
  redraw() {
    if (emu) sendFrame(true);
  },
};

// メッセージは順に処理する（boot の途中で input が来ても順序を保つ）。
let queue = Promise.resolve();
onmessage = ({ data }) => {
  queue = queue.then(async () => {
    try {
      await handlers[data.op](data);
    } catch (e) {
      if (e instanceof WebAssembly.RuntimeError) fatal(e);
      else post({ error: `${data.op}: ${e?.message ?? e}` });
    }
  });
};
