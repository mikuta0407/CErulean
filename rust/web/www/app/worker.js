// ブラウザ版のエミュレーション Worker（計画書 §3.4・§7.2〜7.4）。
//
// wasm のマシンを 1 台持ち、実時間に合わせて小さな単位で進める。1 単位ごとに制御を
// 返し（MessageChannel）、その間にメインからの入力を受け取る。入力は受け取った時点
// （run の合間 = 命令境界）で emu.input に渡す。画面は変化したときだけ RGBA を
// transfer で送る。壁時計（performance.now・Date）を見るのはここと UI だけで、
// ゲストの時刻は命令数から決まる（決定論性）。
//
// 保存は OPFS（Worker の同期アクセスハンドル）。スナップショットは gzip して置く
// （無圧縮 約 134MB → 約 25MB。計画書の段階2 の計測）。
//   images/<sha256>.bin・.json   読み込んだイメージ（SHA-256 がキー）
//   saves/<名前>.snap.gz・.json  自動保存（auto-0〜2 のリング）と手動保存
//   rec/start.snap.gz・script.txt  最後の記録（起点スナップショットとスクリプト）
// 書き込みは一時ファイルに書き終えてから名前を変える（書きかけを残さない。§7.3）。
import init, * as wasm from "../pkg/cerulean_web.js";

const IPS = 135_200_000; // 仮想時間 1 秒あたりの命令数（smdk2410 の INSTRUCTIONS_PER_SECOND）
// 1 回の run で進める仮想時間（10ms）。入力の反映の遅れはこれ以下になる。
const SLICE = IPS / 100;
// 1 回の tick で実行に使う壁時計の上限（ms）。これを過ぎたら制御を返す。
const BUDGET_MS = 12;
// これより遅れたら追いつくのを諦める（遅い端末で遅れがたまり続けないように）。
const MAX_LAG_SEC = 0.25;
// イメージから起動したとき、この命令数（Today の完成が約 34〜35 億命令目）までは
// 最高速で進め、着いたら自動保存する（計画書 §7.1 の初回起動の体験）。
const BOOT_TURBO_STEPS = 3_600_000_000n;
const FRAME_MS = 33;
const STATUS_MS = 500;
const JIT_PARAMS = [64, 32]; // 計測ページの既定と同じ（閾値・まとめるブロック数）
// 自動保存の間隔（壁時計）。保存の間は 0.5〜1 秒ほど止まるので、操作がなければ間を空ける。
const AUTOSAVE_ACTIVE_MS = 30_000; // 前回の保存の後に入力があったとき
const AUTOSAVE_IDLE_MS = 300_000; // 入力がないとき
const AUTOSAVE_SLOTS = 3;

let wasmMemory = null;
let emu = null;
let imageId = "";
let speed = 1; // 0 = 最高速
let paused = false;
let hidden = false;
let stopped = false; // ゲストが止まった（未実装命令など）か panic した
let broken = false; // panic した（状態が壊れている可能性があるので保存しない。§7.4）
let turboUntil = 0n;
let jitOn = true;
// 実時間との対応の基準（この壁時計の時刻に、この命令数だった）
let baseT = 0;
let baseSteps = 0n;
let scheduled = false;
let lastFrame = null;
let lastFrameT = 0;
let st = { t: 0, steps: 0n, idle: 0n };
let saving = false;
let lastSaveT = 0;
let inputSinceSave = false;
let recStartSteps = null;
// 再開したとき（保存からの再開・非表示からの復帰・起動の早送りの後）にゲストの時計を
// ホストの時刻に合わせるか。合わせるのは記録される入力（rtc）なので決定論は崩れない。
// スナップショットは保存時点の時刻の続きなので、合わせないと実際の日時からずれる（§7.4）。
let syncClock = true;
let hiddenAt = 0;
const uartDec = new TextDecoder("latin1");

const post = (m, transfer) => postMessage(m, transfer ?? []);
const log = (msg) => post({ log: msg });
const hex = (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, "0")).join("");
// crypto.subtle は安全なコンテキスト（HTTPS か localhost）だけ。
const sha256 = async (b) => (globalThis.crypto?.subtle ? hex(await crypto.subtle.digest("SHA-256", b)) : "");

// ---- OPFS ----

async function dir(name) {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(name, { create: true });
}

// 一時ファイルに書き終えてから name に置き換える。data は Uint8Array か ReadableStream。
// 戻り値は書いたバイト数。
async function writeAtomic(d, name, data) {
  const tmpName = name + ".tmp";
  const tmp = await d.getFileHandle(tmpName, { create: true });
  const h = await tmp.createSyncAccessHandle();
  let size = 0;
  try {
    h.truncate(0);
    if (data instanceof Uint8Array) {
      size = h.write(data, { at: 0 });
    } else {
      const r = data.getReader();
      for (;;) {
        const { done, value } = await r.read();
        if (done) break;
        size += h.write(value, { at: size });
      }
    }
    h.flush();
  } catch (e) {
    h.close();
    await d.removeEntry(tmpName).catch(() => {});
    throw e;
  }
  h.close();
  if (tmp.move) {
    await tmp.move(name);
  } else {
    // TODO(Safari): FileSystemHandle.move が無い環境。置き換えが原子的でなくなる
    // （書きかけは .tmp に残るだけなので、壊れたファイルは読み込み時の検査で除く）。
    const dst = await d.getFileHandle(name, { create: true });
    const w = await dst.createSyncAccessHandle();
    const src = await (await tmp.getFile()).arrayBuffer();
    w.truncate(0);
    w.write(new Uint8Array(src), { at: 0 });
    w.flush();
    w.close();
    await d.removeEntry(tmpName);
  }
  return size;
}

const writeJson = (d, name, v) => writeAtomic(d, name, new TextEncoder().encode(JSON.stringify(v)));

async function readJson(d, name) {
  return JSON.parse(await (await (await d.getFileHandle(name)).getFile()).text());
}

async function fileSize(d, name) {
  try {
    return (await (await d.getFileHandle(name)).getFile()).size;
  } catch {
    return -1;
  }
}

// ---- イメージ ----

async function listImages() {
  const d = await dir("images");
  const out = [];
  for await (const [name] of d.entries()) {
    if (!name.endsWith(".json")) continue;
    try {
      const m = await readJson(d, name);
      if ((await fileSize(d, `${m.id}.bin`)) === m.size) out.push(m);
    } catch {
      // 壊れたメタデータは無視する
    }
  }
  return out.sort((a, b) => (b.used ?? 0) - (a.used ?? 0));
}

async function storeImage(bytes, name) {
  const id = await sha256(bytes);
  if (!id) return "";
  try {
    const d = await dir("images");
    await writeAtomic(d, `${id}.bin`, bytes);
    await writeJson(d, `${id}.json`, { id, name, size: bytes.length, used: Date.now() });
  } catch (e) {
    log(`イメージを保存できません: ${e.message}`);
  }
  return id;
}

async function loadStoredImage(id) {
  const d = await dir("images");
  const bytes = new Uint8Array(await (await (await d.getFileHandle(`${id}.bin`)).getFile()).arrayBuffer());
  const meta = await readJson(d, `${id}.json`);
  meta.used = Date.now();
  await writeJson(d, `${id}.json`, meta);
  return { bytes, name: meta.name };
}

// ---- スナップショット ----

// 今の状態を gzip して d/<base>.snap.gz に書く。状態の写しは同期で取る（その間は
// 命令が進まない）。afterCapture は写しを取った直後（同じ命令境界）に呼ぶ。
// 圧縮と書き込みは非同期で、その間もエミュレーションは進む。
async function saveState(d, base, meta, afterCapture) {
  takeUart(); // UART1 の送信バイトはスナップショットに入らないので先に送る
  const cs = new CompressionStream("gzip");
  const w = cs.writable.getWriter();
  const t0 = performance.now();
  const steps = emu.steps();
  const pending = [];
  emu.saveSnapshotTo(imageId, (c) => pending.push(w.write(c)));
  afterCapture?.();
  const captureMs = performance.now() - t0;
  pending.push(w.close());
  const [size] = await Promise.all([writeAtomic(d, `${base}.snap.gz`, cs.readable), ...pending]);
  const m = { ...meta, name: base, imageId, steps: steps.toString(), savedAt: Date.now(), size };
  await writeJson(d, `${base}.json`, m);
  log(`保存 ${base}: 命令 ${steps.toLocaleString()}、${(size / 1e6).toFixed(1)}MB（写し ${captureMs.toFixed(0)}ms・全体 ${(performance.now() - t0).toFixed(0)}ms）`);
  return m;
}

async function readState(d, base) {
  const f = await (await d.getFileHandle(`${base}.snap.gz`)).getFile();
  return gunzipIfNeeded(f);
}

// gzip（1F 8B）なら展開する。書き出したファイルと、CLI の無圧縮のファイルの両方を読む。
async function gunzipIfNeeded(blob) {
  const head = new Uint8Array(await blob.slice(0, 2).arrayBuffer());
  const s = head[0] === 0x1f && head[1] === 0x8b ? blob.stream().pipeThrough(new DecompressionStream("gzip")) : blob.stream();
  return new Uint8Array(await new Response(s).arrayBuffer());
}

// 無圧縮のスナップショットから新しいマシンを作る。失敗したらそのマシンは捨てる。
function machineFromSnapshot(raw) {
  const e = new wasm.Emu();
  let off = 0;
  try {
    const id = e.loadSnapshotFrom((buf) => {
      const n = Math.min(buf.length, raw.length - off);
      buf.set(raw.subarray(off, off + n));
      off += n;
      return n;
    });
    return { e, id };
  } catch (err) {
    e.free();
    throw err;
  }
}

async function listSaves() {
  const d = await dir("saves");
  const out = [];
  for await (const [name] of d.entries()) {
    if (!name.endsWith(".json")) continue;
    try {
      const m = await readJson(d, name);
      // 長さが合わないもの（書きかけ・消えたもの）は除く。中身の検査は読み込み時（CRC）。
      if ((await fileSize(d, `${m.name}.snap.gz`)) === m.size) out.push(m);
    } catch {}
  }
  return out.sort((a, b) => b.savedAt - a.savedAt);
}

async function sendLists() {
  let images = [];
  let saves = [];
  let estimate = null;
  try {
    images = await listImages();
    saves = await listSaves();
    estimate = await navigator.storage.estimate?.();
  } catch (e) {
    log(`OPFS を使えません: ${e.message}`);
  }
  post({ images, saves, estimate: estimate && { usage: estimate.usage, quota: estimate.quota } });
}

async function autosave(reason) {
  if (!emu || broken || stopped || saving) return;
  saving = true;
  post({ saving: true });
  try {
    const d = await dir("saves");
    const saves = (await listSaves()).filter((s) => s.kind === "auto");
    // 空いている枠か、いちばん古い枠に書く
    const used = new Map(saves.map((s) => [s.name, s.savedAt]));
    let slot = null;
    for (let i = 0; i < AUTOSAVE_SLOTS; i++) {
      const n = `auto-${i}`;
      if (!used.has(n)) {
        slot = n;
        break;
      }
      if (slot === null || used.get(n) < used.get(slot)) slot = n;
    }
    await saveState(d, slot, { kind: "auto", label: reason });
    lastSaveT = performance.now();
    inputSinceSave = false;
    await sendLists();
  } catch (e) {
    // 容量超過など。失敗は必ず知らせる（§7.4）。書きかけは writeAtomic が消す。
    post({ error: `自動保存に失敗しました: ${e.message}`, soft: true });
  } finally {
    saving = false;
    post({ saving: false });
    if (emu) rebase(); // 保存で止まった分を追いつこうとしない
  }
}

// ---- ゲストの時計 ----

function rtcNow() {
  const d = new Date();
  return [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes(), d.getSeconds()];
}

function setClockNow(reason) {
  if (!emu || stopped) return;
  const t = rtcNow();
  emu.setClock(Int32Array.from(t));
  log(`時計を合わせた（${reason}）: ${t[0]}-${String(t[1]).padStart(2, "0")}-${String(t[2]).padStart(2, "0")} ${t.slice(3).map((v) => String(v).padStart(2, "0")).join(":")}`);
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
      if (turboUntil && emu.steps() >= turboUntil) {
        turboUntil = 0n;
        // 早送りの間は仮想時間が実時間より速く進んだので合わせ直す
        if (syncClock) setClockNow("起動の早送りの後");
        enqueue(() => autosave("Today"));
      }
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
    if (!saving && turboUntil === 0n && now - lastSaveT >= (inputSinceSave ? AUTOSAVE_ACTIVE_MS : AUTOSAVE_IDLE_MS)) {
      lastSaveT = now; // 失敗しても次は間を空ける
      enqueue(() => autosave("定期"));
    }
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

function takeUart() {
  const uart = emu.takeUart();
  if (uart.length) post({ uart: uartDec.decode(uart) });
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
    recording: emu.recording(),
  };
  takeUart();
  const js = JSON.parse(emu.jitStats());
  if (js.error) s.jitError = js.error;
  s.jitModules = js.modules;
  st = { t: now, steps, idle };
  post({ status: s });
}

// wasm の panic（RuntimeError）はインスタンスを使えなくする（§6.2）。以後は動かさず、
// 保存もしない（壊れた状態で自動保存を上書きしないため）。
function fatal(e) {
  stopped = true;
  broken = true;
  post({ error: `${e?.name ?? "Error"}: ${e?.message ?? e}`, fatal: true });
}

function start(e, id, name, turbo) {
  emu?.free();
  emu = e;
  imageId = id;
  if (jitOn) emu.setJit(true, ...JIT_PARAMS);
  stopped = false;
  paused = false;
  lastFrame = null;
  recStartSteps = null;
  turboUntil = turbo && emu.steps() < BOOT_TURBO_STEPS ? BOOT_TURBO_STEPS : 0n;
  st = { t: 0, steps: 0n, idle: 0n };
  lastSaveT = performance.now();
  inputSinceSave = false;
  rebase();
  post({ booted: { imageId, name, steps: emu.steps().toString() } });
  sendFrame(true);
  schedule();
}

function bootImage(bytes, name, id, rtc) {
  const e = new wasm.Emu();
  try {
    e.loadImage(bytes, name, Int32Array.from(rtc));
  } catch (err) {
    e.free();
    throw err;
  }
  start(e, id, name, true);
}

// 記録の終わり: スクリプトに、終わりの時点のゲストから見える値（一致確認と同じ
// CPU 状態・RAM・画面の SHA-256）と、その時点の画面の保存・終了を書き足す。
// CLI で起点スナップショットから再生すると、同じ命令数で同じ値になるはず。
async function finishRecording() {
  const steps = emu.steps();
  const base = `cerulean-rec-${recStartSteps}`;
  let script = emu.recordStop(`${base}.snap`, imageId);
  const screen = emu.frame();
  const cpuDump = emu.cpuDump();
  // RAM は wasm のメモリの view で渡す（emu.ram() は wasm の中で 128MB の複製を作る。
  // digest は呼んだ時点で中身を写すので、view は直後に無効になってもよい）
  const ram = new Uint8Array(wasmMemory.buffer, emu.ramPtr(), 128 << 20);
  const [cpuH, ramH, scrH] = await Promise.all([sha256(cpuDump), sha256(ram), sha256(screen)]);
  script +=
    `# end step: ${steps}\n` +
    `# expected at the end: cpu_sha256 ${cpuH} ram_sha256 ${ramH} screen_sha256 ${scrH}\n` +
    `# replay: gunzip ${base}.snap.gz && cerulean run --snap-load ${base}.snap --script ${base}.txt --result result.jsonl <image>\n` +
    `@${steps}i shot ${base}-end.png\n` +
    `@${steps}i quit\n`;
  const d = await dir("rec");
  await writeAtomic(d, "script.txt", new TextEncoder().encode(script));
  await writeJson(d, "rec.json", { base, startSteps: recStartSteps.toString(), steps: steps.toString(), imageId });
  recStartSteps = null;
  post({ recorded: { base, script } });
}

async function sendFile(d, file, downloadName) {
  const blob = await (await d.getFileHandle(file)).getFile();
  const bytes = new Uint8Array(await blob.arrayBuffer());
  post({ download: { name: downloadName, bytes } }, [bytes.buffer]);
}

const handlers = {
  async init() {
    wasmMemory = (await init()).memory;
    wasm.installPanicHook();
    post({ ready: true });
    await sendLists();
  },
  async bootFile({ bytes, name, rtc }) {
    const id = await storeImage(bytes, name);
    bootImage(bytes, name, id, rtc);
    await sendLists();
  },
  async bootStored({ id, rtc }) {
    const { bytes, name } = await loadStoredImage(id);
    bootImage(bytes, name, id, rtc);
  },
  async deleteImage({ id }) {
    const d = await dir("images");
    for (const n of [`${id}.bin`, `${id}.json`]) await d.removeEntry(n).catch(() => {});
    await sendLists();
  },
  // 保存から再開する。name を省くと、自動・手動を問わず新しい順に試し、壊れたものは
  // 飛ばす（1 つ前の世代に戻る。§7.3）。
  async resume({ name }) {
    const d = await dir("saves");
    const cands = name ? [name] : (await listSaves()).map((s) => s.name);
    if (!cands.length) throw new Error("保存がありません");
    for (const n of cands) {
      try {
        const meta = await readJson(d, `${n}.json`);
        const { e, id } = machineFromSnapshot(await readState(d, n));
        start(e, id, `${meta.kind === "auto" ? "自動保存" : "保存"} ${n}`, true);
        if (syncClock && turboUntil === 0n) setClockNow("再開");
        return;
      } catch (err) {
        log(`${n} を読めません（壊れている可能性）: ${err.message}`);
      }
    }
    throw new Error("読める保存がありません");
  },
  async save() {
    if (!emu || broken) return;
    saving = true;
    post({ saving: true });
    try {
      const d = await dir("saves");
      await saveState(d, `save-${Date.now()}`, { kind: "manual", label: "手動" });
      await sendLists();
    } finally {
      saving = false;
      post({ saving: false });
      rebase();
    }
  },
  async autosave({ reason }) {
    await autosave(reason);
  },
  async deleteSave({ name }) {
    const d = await dir("saves");
    for (const n of [`${name}.snap.gz`, `${name}.json`]) await d.removeEntry(n).catch(() => {});
    await sendLists();
  },
  async exportSave({ name }) {
    const d = await dir("saves");
    const m = await readJson(d, `${name}.json`);
    await sendFile(d, `${name}.snap.gz`, `cerulean-${m.steps}.snap.gz`);
  },
  // 今の状態を手動保存に加えて書き出す（止まったときの不具合報告用）。
  async exportCurrent() {
    if (!emu || broken) return;
    const d = await dir("saves");
    const m = await saveState(d, `save-${Date.now()}`, { kind: "manual", label: "停止時" });
    await sendLists();
    await sendFile(d, `${m.name}.snap.gz`, `cerulean-${m.steps}.snap.gz`);
  },
  // 書き出したスナップショット（gzip か無圧縮）を読み込んで再開し、手動保存に加える。
  async importSnapshot({ bytes }) {
    const raw = await gunzipIfNeeded(new Blob([bytes]));
    const { e, id } = machineFromSnapshot(raw);
    start(e, id, "読み込んだスナップショット", false);
    if (syncClock) setClockNow("読み込み");
    await handlers.save();
  },
  async recordStart() {
    if (!emu || stopped || emu.recording()) return;
    const d = await dir("rec");
    // 起点のスナップショットと記録の開始を同じ命令境界にする
    await saveState(d, "start", { kind: "rec" }, () => {
      emu.recordStart();
      recStartSteps = emu.steps();
    });
    sendStatus(performance.now());
  },
  async recordStop() {
    if (!emu || !emu.recording()) return;
    await finishRecording();
    sendStatus(performance.now());
  },
  async exportRecording({ what }) {
    const d = await dir("rec");
    const m = await readJson(d, "rec.json");
    if (what === "script") await sendFile(d, "script.txt", `${m.base}.txt`);
    else await sendFile(d, "start.snap.gz", `${m.base}.snap.gz`);
  },
  input({ t, x = 0, y = 0, key = "" }) {
    if (!emu || stopped) return;
    emu.input(t, x, y, key);
    inputSinceSave = true;
    // 止まっているときも入力の直後の画面を見せる
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
  // 非表示中は止めて自動保存する（ブラウザがタイマーを絞るため。決定論的なので止めても
  // 結果は変わらない。iOS は通知なしに落とすことがあるので、確実なのは定期保存。§7.4）
  async visibility({ hidden: h }) {
    hidden = h;
    if (!emu) return;
    if (h) {
      hiddenAt = Date.now();
      await autosave("非表示");
    } else {
      // 止めていた間の分だけゲストの時計が遅れるので合わせる（短い切り替えは無視する）
      if (syncClock && turboUntil === 0n && Date.now() - hiddenAt > 2000) setClockNow("復帰");
      rebase();
      schedule();
    }
  },
  speed({ v }) {
    speed = v;
    if (emu) rebase();
  },
  clockOption({ on }) {
    syncClock = on;
  },
  syncClock() {
    setClockNow("手動");
  },
  skipTurbo() {
    turboUntil = 0n;
    if (emu) rebase();
  },
  jit({ on }) {
    jitOn = on;
    if (emu) emu.setJit(on, ...JIT_PARAMS);
  },
  redraw() {
    if (emu) sendFrame(true);
  },
};

// メッセージと自動保存は 1 本の列で順に処理する（保存の途中で読み込みが来ても順序を保つ）。
let queue = Promise.resolve();
function enqueue(f, op = "") {
  queue = queue.then(async () => {
    try {
      await f();
    } catch (e) {
      if (e instanceof WebAssembly.RuntimeError) fatal(e);
      else post({ error: `${op}: ${e?.message ?? e}`, soft: true });
    }
  });
}
onmessage = ({ data }) => {
  // 入力は列に並べず直ちに適用する（保存の圧縮・書き込みを待たせない。適用は
  // どちらでも run の合間 = 命令境界）
  if (data.op === "input") {
    try {
      handlers.input(data);
    } catch (e) {
      post({ error: `input: ${e?.message ?? e}`, soft: true });
    }
    return;
  }
  enqueue(() => handlers[data.op](data), data.op);
};
