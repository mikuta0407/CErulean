// ブラウザ版の UI（メインスレッド）。エミュレーションと保存は worker.js が行い、ここは
// 画面の描画・入力（タッチ・ハードウェアボタン・PC のキー）・操作パネルだけを持つ
// （計画書 §3.4・§7.1）。入力の対応表と座標変換は Go 版の serve の UI
// （www/legacy-serve/app.js）から移したもの。
const $ = (id) => document.getElementById(id);
const canvas = $("screen");
const ctx = canvas.getContext("2d");
// 時計の自動合わせの設定は端末ごとに覚える（保存できない環境では既定の「合わせる」）
try {
  $("clockSync").checked = localStorage.getItem("cerulean-clock-sync") !== "0";
} catch {}
// 音の設定も端末ごと（既定はオフ）
try {
  const a = JSON.parse(localStorage.getItem("cerulean-audio") ?? "{}");
  $("audioOn").checked = !!a.on;
  if (typeof a.vol === "number") $("audioVol").value = a.vol;
} catch {}
// ネットワークの設定も端末ごと（既定はオフ）。中継サーバーの URL の既定は、このページを
// 配信しているサイトの /relay（`cerulean serve --with-relay` が置く）。
const defaultRelay = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/relay`;
try {
  const n = JSON.parse(localStorage.getItem("cerulean-net") ?? "{}");
  $("netOn").checked = !!n.on;
  $("netUrl").value = n.url || defaultRelay;
  $("netToken").value = n.token ?? "";
} catch {
  $("netUrl").value = defaultRelay;
}
// `cerulean serve --with-relay` が表示する URL の #relay-token=… でトークンを入れる
// （入れたら URL から消す。オンにするのはユーザー）
{
  const m = /(?:^#|&)relay-token=([^&]+)/.exec(location.hash);
  if (m) {
    $("netToken").value = decodeURIComponent(m[1]);
    if (!$("netUrl").value) $("netUrl").value = defaultRelay;
    try {
      const n = JSON.parse(localStorage.getItem("cerulean-net") ?? "{}");
      localStorage.setItem("cerulean-net", JSON.stringify({ ...n, url: $("netUrl").value, token: $("netToken").value }));
    } catch {}
    history.replaceState(null, "", location.pathname + location.search);
  }
}

let worker = null;
const send = (op, args = {}, transfer = []) => worker?.postMessage({ op, ...args }, transfer);

function log(msg) {
  const el = $("log");
  el.textContent = (`${new Date().toLocaleTimeString()} ${msg}\n` + el.textContent).slice(0, 20000);
}

let booted = false;
let stopped = false;
let fatal = false;
let paused = false;
let recording = false;
let images = [];
let saves = [];
// panic から読み直したときは、そのまま直前の自動保存から再開する（一覧が届いたら）。
let autoResume = sessionStorage.getItem("cerulean-autoresume") === "1";
sessionStorage.removeItem("cerulean-autoresume");

// ---- 起動: 1 タブだけ（§7.4）----
// 同じサイトを 2 つのタブで開くと自動保存が衝突するので、Web Locks でエミュレータを
// 動かすタブを 1 つにする。取れなければ待ち、前のタブが閉じたら動き出す。
if (!("WebAssembly" in globalThis)) {
  $("startMsg").textContent = "このブラウザでは WebAssembly が使えません（iOS のロックダウンモードでは無効になります）。";
} else if (navigator.locks) {
  navigator.locks.request("cerulean-emulator", { ifAvailable: true }, (lock) => {
    if (lock) return startWorker();
    $("start").hidden = true;
    $("otherTab").hidden = false;
    return navigator.locks.request("cerulean-emulator", () => {
      $("otherTab").hidden = true;
      $("start").hidden = false;
      return startWorker();
    });
  });
} else {
  startWorker();
}

// Worker を動かし、ロックを持ち続ける（戻り値の Promise が解決しない限り持つ）。
function startWorker() {
  worker = new Worker("worker.js", { type: "module" });
  worker.onmessage = ({ data }) => onWorker(data);
  worker.onerror = (e) => {
    log(`Worker の異常終了: ${e.message}`);
    showStop(`Worker が異常終了しました: ${e.message}`, true);
  };
  send("init");
  send("clockOption", { on: $("clockSync").checked });
  send("audio", { on: $("audioOn").checked });
  sendNet(false);
  return new Promise(() => {});
}

// ---- Worker からのメッセージ ----
function onWorker(d) {
  if (d.log) log(d.log);
  if (d.ready) $("startMsg").textContent = "";
  if (d.images) {
    images = d.images;
    saves = d.saves;
    renderLists(d.estimate);
    if (autoResume) {
      autoResume = false;
      send("resume", {});
    }
  }
  if (d.booted) {
    // 最初の絵が出るまではブート画面（再開ならすぐ絵が届いて消える）
    sawPicture = false;
    bootSteps = Number(d.booted.steps);
    bootLines = [];
    booted = true;
    stopped = false;
    fatal = false;
    $("start").hidden = true;
    $("stopBanner").hidden = true;
    $("pause").disabled = false;
    $("menu").hidden = true;
    log(`起動: ${d.booted.name}（命令 ${BigInt(d.booted.steps).toLocaleString()}）`);
    if (cardInfo) renderCard(cardInfo);
    // 保存を消されにくくする（§7.3。許可されるかはブラウザが決める）
    navigator.storage?.persist?.().catch(() => {});
  }
  if (d.frame) drawFrame(d.frame, d.w, d.h);
  if (d.audio) playAudio(d.audio, d.rate);
  if (d.status) showStatus(d.status);
  if (d.uart) {
    bootLog(d.uart);
    const el = $("uart");
    el.textContent = (el.textContent + d.uart).slice(-50000);
  }
  if ("saving" in d) $("saveBadge").hidden = !d.saving;
  if (d.stopped) showStop(`エミュレーションが止まりました。\n${d.stopped}`, false);
  if (d.recorded) {
    $("recText").textContent = d.recorded.script;
    $("recText").hidden = false;
    $("recScript").hidden = false;
    $("recSnap").hidden = false;
    $("recCards").hidden = !/^@\d+i (card|share) insert /m.test(d.recorded.script);
    log(`記録を止めました: ${d.recorded.base}`);
  }
  if (d.download) download(d.download.bytes, d.download.name);
  if (d.card) renderCard(d.card);
  if (d.net) {
    const n = d.net;
    $("netState").textContent = n.on ? `${n.msg}${n.nic ? "" : "（カード未挿入）"}・接続 ${n.conns}` : n.msg;
  }
  if (d.error) {
    log("エラー: " + d.error);
    if (d.fatal) showStop(d.error, true);
    else if (!d.soft || !booted) alert(d.error);
  }
}

function showStop(text, isFatal) {
  stopped = true;
  fatal ||= isFatal;
  $("stopText").textContent = text;
  $("stopExport").hidden = fatal; // panic 後の状態は壊れている可能性があるので書き出さない
  $("stopBanner").hidden = false;
}

function download(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes]));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

// ---- 画面 ----
function drawFrame(buf, w, h) {
  if (!w || !h) {
    if (!sawPicture) return drawBoot();
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    return;
  }
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
    fitScreen();
  }
  if (!sawPicture && isBlack(buf)) return drawBoot();
  sawPicture = true;
  ctx.putImageData(new ImageData(new Uint8ClampedArray(buf), w, h), 0, 0);
}

// ---- ブート画面 ----
// イメージから起動すると、Windows Mobile のスプラッシュ（約 22 億命令目）までゲストの画面は
// 真っ暗なので、その間はここで描いた画面を見せる（UI だけ。ゲストの状態には触れない）。
// 起動（または再開）してから最初に絵が出るまでの間だけで、以後ゲストが画面を黒くしても出さない。
const SPLASH_STEPS = 2_200_000_000; // PPC_USA でスプラッシュが出る頃（進み具合の目安）
let sawPicture = true; // 起動してから、真っ暗でない画面が出たか
let bootSteps = 0;
let bootLines = []; // カーネルのデバッグ出力（UART1）の最後の数行

function isBlack(buf) {
  const px = new Uint32Array(buf);
  for (let i = 0; i < px.length; i++) if (px[i] & 0x00ffffff) return false;
  return true;
}

function drawBoot() {
  const w = canvas.width;
  const h = canvas.height;
  const g = ctx.createLinearGradient(0, 0, 0, h);
  g.addColorStop(0, "#0b1a33");
  g.addColorStop(1, "#000");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  ctx.textAlign = "center";
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = "#7fa7d9";
  ctx.font = "10px sans-serif";
  ctx.fillText("CErulean", w / 2, h * 0.3);
  ctx.fillStyle = "#fff";
  ctx.font = "bold 17px sans-serif";
  ctx.fillText("Windows Mobile 5.0", w / 2, h * 0.3 + 26);
  ctx.fillStyle = "#b8c4d6";
  ctx.font = "11px sans-serif";
  ctx.fillText("起動しています…", w / 2, h * 0.3 + 46);
  // 進み具合（スプラッシュまでの命令数の目安。イメージによって前後する）
  const p = Math.min(bootSteps / SPLASH_STEPS, 0.99);
  const bw = Math.round(w * 0.6);
  const bx = Math.round((w - bw) / 2);
  const by = Math.round(h * 0.55);
  ctx.fillStyle = "#1c2c47";
  ctx.fillRect(bx, by, bw, 5);
  ctx.fillStyle = "#3d8ee8";
  ctx.fillRect(bx, by, Math.round(bw * p), 5);
  ctx.fillStyle = "#6f7f96";
  ctx.font = "9px sans-serif";
  ctx.fillText(`${Math.floor(p * 100)}%`, w / 2, by + 18);
  // カーネルのデバッグ出力の最後の数行
  ctx.textAlign = "left";
  ctx.font = "8px monospace";
  ctx.fillStyle = "#4f6178";
  bootLines.forEach((line, i) => {
    let t = line;
    while (t.length > 1 && ctx.measureText(t).width > w - 8) t = t.slice(0, -1);
    ctx.fillText(t, 4, h - 6 - (bootLines.length - 1 - i) * 10);
  });
}

function bootLog(text) {
  const lines = text.split("\n").map((l) => l.trim()).filter(Boolean);
  if (!lines.length) return;
  bootLines = [...bootLines, ...lines].slice(-4);
  if (!sawPicture) drawBoot();
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

// ---- 音 ----
// Worker から届いたサンプル（16 ビット・左右交互）を、前の続きの時刻に並べて鳴らす。
// 届く間隔は DMA の 1 区切り（約 12ms）と Worker の実行の単位でばらつくので、少し先
// （AUDIO_LEAD）から始めて吸収する。遅れて前の続きが過ぎていたら（間の無音・実行の
// 遅れ）、今から AUDIO_LEAD 先に置き直す。先へ行き過ぎたら（Worker が実時間より速く
// 進めた分）置き直して追いつかせる。
const AUDIO_LEAD = 0.08;
const AUDIO_MAX_AHEAD = 0.5;
let actx = null;
let gain = null;
let audioNext = 0;

// AudioContext はユーザーの操作の中でしか始められない（自動再生の制限）ので、
// オンにしたときと、画面・ボタンの操作のたびに作る・再開する。
function audioStart() {
  if (!$("audioOn").checked) return;
  if (!actx) {
    try {
      actx = new AudioContext();
    } catch (e) {
      log(`音を出せません: ${e.message}`);
      $("audioOn").checked = false;
      return;
    }
    gain = actx.createGain();
    gain.connect(actx.destination);
  }
  gain.gain.value = +$("audioVol").value;
  if (actx.state === "suspended") actx.resume().catch(() => {});
}

function playAudio(buf, rate) {
  if (!actx || actx.state !== "running" || !$("audioOn").checked || !rate) return;
  const s = new Int16Array(buf);
  const n = s.length >> 1;
  if (!n) return;
  const ab = actx.createBuffer(2, n, rate);
  const l = ab.getChannelData(0);
  const r = ab.getChannelData(1);
  for (let i = 0; i < n; i++) {
    l[i] = s[2 * i] / 32768;
    r[i] = s[2 * i + 1] / 32768;
  }
  const now = actx.currentTime;
  if (audioNext < now + 0.01 || audioNext > now + AUDIO_MAX_AHEAD) audioNext = now + AUDIO_LEAD;
  const src = actx.createBufferSource();
  src.buffer = ab;
  src.connect(gain);
  src.start(audioNext);
  audioNext += n / rate;
}

function saveAudioCfg() {
  try {
    localStorage.setItem("cerulean-audio", JSON.stringify({ on: $("audioOn").checked, vol: +$("audioVol").value }));
  } catch {}
}
$("audioOn").onchange = () => {
  saveAudioCfg();
  send("audio", { on: $("audioOn").checked });
  if ($("audioOn").checked) audioStart();
  else actx?.suspend().catch(() => {});
};
$("audioVol").oninput = () => {
  saveAudioCfg();
  if (gain) gain.gain.value = +$("audioVol").value;
};
for (const ev of ["pointerdown", "keydown"]) window.addEventListener(ev, audioStart, { capture: true });

// ---- 状態表示 ----
function fmtTime(sec) {
  const m = Math.floor(sec / 60);
  return `${m}:${(sec - m * 60).toFixed(1).padStart(4, "0")}`;
}
function showStatus(s) {
  paused = s.paused;
  recording = s.recording;
  $("pause").classList.toggle("on", paused);
  $("pause").textContent = paused ? "▶" : "❚❚";
  $("pause").title = paused ? "再開" : "一時停止";
  $("steps").textContent = BigInt(s.steps).toLocaleString();
  bootSteps = Number(s.steps);
  if (!sawPicture) drawBoot();
  $("vtime").textContent = fmtTime(s.virtualSec);
  $("ratio").textContent = paused ? "一時停止中" : `${s.ratio.toFixed(2)} 倍`;
  $("mips").textContent = `${s.mips.toFixed(1)}M 命令/秒`;
  $("idle").textContent = `${(s.idle * 100).toFixed(1)}% をスキップ`;
  const badge = $("badge");
  badge.hidden = !(s.turbo && !paused) && !s.jitError;
  badge.classList.toggle("warn", !!s.jitError);
  badge.textContent = s.jitError ? "JIT 停止" : "早送り";
  badge.title = s.jitError ?? "";
  $("skipTurbo").hidden = !(s.turbo && s.speed !== 0);
  $("recBadge").hidden = !recording;
  $("rec").textContent = recording ? "記録停止" : "記録開始";
  $("rec").classList.toggle("on", recording);
  $("statusLine").textContent = paused
    ? "一時停止中"
    : `${fmtTime(s.virtualSec)}  ×${s.ratio.toFixed(2)}  ${s.mips.toFixed(0)}M/s`;
}

// ---- 最初の画面・保存の一覧 ----
// ゲストの RTC はホストのローカル時刻から（イメージから起動したときだけ。以後と
// スナップショットからの再開では、ゲストの時計は命令数で進む。§7.4）。
function rtcNow() {
  const d = new Date();
  return [d.getFullYear(), d.getMonth() + 1, d.getDate(), d.getHours(), d.getMinutes(), d.getSeconds()];
}

const fmtSteps = (s) => `${(Number(s) / 1e9).toFixed(2)}G 命令`;
const fmtDate = (t) => new Date(t).toLocaleString();

function item(label, sub, onclick, extra = []) {
  const li = document.createElement("li");
  const b = document.createElement("button");
  b.className = "boot";
  const span = document.createElement("span");
  span.textContent = label;
  const small = document.createElement("small");
  small.textContent = sub;
  b.append(span, small);
  b.onclick = onclick;
  li.append(b, ...extra);
  return li;
}
function smallButton(text, title, onclick) {
  const b = document.createElement("button");
  b.className = "del";
  b.textContent = text;
  b.title = title;
  b.onclick = onclick;
  return b;
}

function renderLists(estimate) {
  const manual = saves.filter((s) => s.kind === "manual");
  const latest = saves[0]; // 「続きから再開」は自動・手動を問わずいちばん新しい保存
  $("resume").hidden = !latest;
  if (latest) $("resumeInfo").textContent = `${fmtDate(latest.savedAt)}・${fmtSteps(latest.steps)}`;

  const sl = $("saveList");
  sl.replaceChildren(
    ...manual.map((s) =>
      item(`保存から再開`, `${fmtDate(s.savedAt)}・${fmtSteps(s.steps)}`, () => send("resume", { name: s.name })),
    ),
  );
  const il = $("imageList");
  il.replaceChildren(
    ...images.map((im) =>
      item(`${im.name} から起動`, `${(im.size / 1e6).toFixed(1)}MB・${im.id.slice(0, 12)}…`, () => send("bootStored", { id: im.id, rtc: rtcNow() }), [
        smallButton("✕", "この端末から消す", () => confirm(`${im.name} をこの端末から消しますか？`) && send("deleteImage", { id: im.id })),
      ]),
    ),
  );
  if (!images.length && !saves.length) $("startMsg").textContent = "イメージを選んでください。";

  const ml = $("menuSaves");
  ml.replaceChildren(
    ...saves.map((s) =>
      item(`${s.kind === "auto" ? "自動" : "手動"}・${fmtSteps(s.steps)}`, `${fmtDate(s.savedAt)}・${(s.size / 1e6).toFixed(1)}MB${s.label ? "・" + s.label : ""}`, () => {
        if (confirm("この保存から再開しますか？（今の状態は失われます。必要なら先に保存してください）")) send("resume", { name: s.name });
      }, [
        smallButton("⤓", "書き出す", () => send("exportSave", { name: s.name })),
        smallButton("✕", "消す", () => confirm("この保存を消しますか？") && send("deleteSave", { name: s.name })),
      ]),
    ),
  );
  if (estimate?.quota) {
    $("storageInfo").textContent = `この端末の保存領域: ${(estimate.usage / 1e6).toFixed(0)}MB 使用 / 上限 ${(estimate.quota / 1e9).toFixed(1)}GB`;
  }
}

$("imageFile").addEventListener("change", async (e) => {
  const f = e.target.files[0];
  e.target.value = "";
  if (!f) return;
  const bytes = new Uint8Array(await f.arrayBuffer());
  send("bootFile", { bytes, name: f.name, rtc: rtcNow() }, [bytes.buffer]);
});
for (const id of ["snapFile", "snapFile2"]) {
  $(id).addEventListener("change", async (e) => {
    const f = e.target.files[0];
    e.target.value = "";
    if (!f) return;
    const bytes = new Uint8Array(await f.arrayBuffer());
    send("importSnapshot", { bytes }, [bytes.buffer]);
  });
}
$("resume").onclick = () => send("resume", {});

// panic の後は wasm のインスタンスが使えないので、ページを読み直してから再開する。
$("stopResume").onclick = () => {
  if (fatal) {
    sessionStorage.setItem("cerulean-autoresume", "1");
    location.reload();
  } else {
    send("resume", {});
  }
};
$("stopExport").onclick = () => send("exportCurrent");
$("stopBack").onclick = () => {
  if (fatal) return location.reload();
  $("stopBanner").hidden = true;
  $("start").hidden = false;
};
$("toStart").onclick = () => {
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
$("clockSync").onchange = () => {
  try {
    localStorage.setItem("cerulean-clock-sync", $("clockSync").checked ? "1" : "0");
  } catch {}
  send("clockOption", { on: $("clockSync").checked });
};
$("clockNow").onclick = () => booted && send("syncClock");
$("saveNow").onclick = () => booted && send("save");
$("rec").onclick = () => booted && send(recording ? "recordStop" : "recordStart");
$("recScript").onclick = () => send("exportRecording", { what: "script" });
$("recSnap").onclick = () => send("exportRecording", { what: "snapshot" });
$("recCards").onclick = () => send("exportRecording", { what: "cards" });

// ---- ネットワーク ----
function sendNet(save = true) {
  const cfg = { on: $("netOn").checked, url: $("netUrl").value.trim(), token: $("netToken").value };
  if (save) {
    try {
      localStorage.setItem("cerulean-net", JSON.stringify(cfg));
    } catch {}
  }
  send("netConfig", cfg);
}
$("netOn").onchange = () => {
  if ($("netOn").checked && !$("netUrl").value.trim()) {
    alert("中継サーバーの URL を入れてください");
    $("netOn").checked = false;
    return;
  }
  sendNet();
};
$("netUrl").onchange = () => sendNet();
$("netCaExport").onclick = () => send("netExportCa");
$("netToken").onchange = () => sendNet();

// ---- ストレージカード ----
// 中身の出し入れは Worker（wasm の CardImage）が行う。ここは一覧の表示と操作だけ。
let cardInfo = null;
const fmtSize = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}MB` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}KB` : `${n}B`);

function renderCard(c) {
  cardInfo = c;
  const sel = $("cardSize");
  if (!sel.options.length) {
    for (const mb of c.sizes) sel.add(new Option(`${mb}MB`, mb, false, mb === 64));
  }
  const has = !!c.meta;
  $("cardState").textContent = c.inserted
    ? `挿しています（${has ? c.meta.name : "カード"}${c.share ? "・フォルダ共有の方式" : ""}）。中身はエミュレータの中にあります。`
    : has
      ? `抜いています: ${c.meta.name}（${fmtSize(c.meta.size)}・空き ${fmtSize(c.free ?? 0)}）`
      : "カードがありません。「作る」で空のカードを作るか、イメージを読み込んでください。";
  $("cardInsert").disabled = c.inserted || !has || !booted || stopped;
  $("cardEject").disabled = !c.inserted;
  $("cardExport").disabled = !has && !c.inserted;
  $("cardNew").disabled = c.inserted;
  $("cardImportFile").disabled = c.inserted;
  $("cardFiles").hidden = c.inserted || !c.entries;
  if (!c.entries) return;
  $("cardPath").textContent = `Storage Card\\${c.path.replaceAll("/", "\\")}`;
  const rows = [];
  if (c.path) {
    const up = c.path.split("/").slice(0, -1).join("/");
    rows.push(item("..", "上のフォルダ", () => send("cardOpen", { path: up })));
  }
  for (const e of c.entries) {
    const t = e.modified;
    const when = `${t[0]}-${String(t[1]).padStart(2, "0")}-${String(t[2]).padStart(2, "0")} ${String(t[3]).padStart(2, "0")}:${String(t[4]).padStart(2, "0")}`;
    const open = e.dir
      ? () => send("cardOpen", { path: c.path ? `${c.path}/${e.name}` : e.name })
      : () => send("cardGet", { name: e.name });
    rows.push(
      item(e.dir ? `📁 ${e.name}` : e.name, e.dir ? when : `${fmtSize(e.size)}・${when}`, open, [
        smallButton("✕", "カードから消す", () => confirm(`${e.name} を消しますか？${e.dir ? "（中身ごと）" : ""}`) && send("cardDelete", { name: e.name })),
      ]),
    );
  }
  $("cardList").replaceChildren(...rows);
}

// files: FileList か File の配列。フォルダごとのときは webkitRelativePath を使う。
async function putCardFiles(files) {
  const list = [];
  const transfer = [];
  for (const f of files) {
    const bytes = new Uint8Array(await f.arrayBuffer());
    list.push({ path: f.webkitRelativePath || f.name, bytes, mtime: f.lastModified });
    transfer.push(bytes.buffer);
  }
  if (list.length) send("cardPut", { files: list }, transfer);
}
$("cardNew").onclick = () => {
  if (cardInfo?.meta && !confirm("今のカードの中身は消えます。新しいカードを作りますか？（必要なら先にイメージを書き出してください）")) return;
  send("cardNew", { mb: +$("cardSize").value });
};
$("cardInsert").onclick = () => send("cardInsert");
$("cardEject").onclick = () => send("cardEject");
$("cardExport").onclick = () => send("cardExport");
$("cardMkdir").onclick = () => {
  const name = prompt("フォルダの名前");
  if (name) send("cardMkdir", { name });
};
$("cardImportFile").addEventListener("change", async (e) => {
  const f = e.target.files[0];
  e.target.value = "";
  if (!f) return;
  if (cardInfo?.meta && !confirm("今のカードをこのイメージで置き換えますか？")) return;
  const bytes = new Uint8Array(await f.arrayBuffer());
  send("cardImport", { bytes, name: f.name }, [bytes.buffer]);
});
for (const id of ["cardAddFiles", "cardAddDir"]) {
  $(id).addEventListener("change", async (e) => {
    const files = [...e.target.files];
    e.target.value = "";
    await putCardFiles(files);
  });
}
const cardDrop = $("cardFiles");
cardDrop.addEventListener("dragover", (e) => {
  e.preventDefault();
  cardDrop.classList.add("drop");
});
cardDrop.addEventListener("dragleave", () => cardDrop.classList.remove("drop"));
cardDrop.addEventListener("drop", async (e) => {
  e.preventDefault();
  cardDrop.classList.remove("drop");
  // TODO: ドロップしたフォルダの中身（webkitGetAsEntry）は未対応（ファイルだけ入れる）
  await putCardFiles([...e.dataTransfer.files].filter((f) => f.size > 0 || f.type));
});

// 非表示・ページを離れるときは止めて自動保存する（§7.4）
document.addEventListener("visibilitychange", () => send("visibility", { hidden: document.hidden }));
window.addEventListener("pagehide", () => send("autosave", { reason: "ページを離れる" }));

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
Object.assign(keyMap, {
  Period: "Period", Comma: "Comma", Slash: "Slash", Minus: "Minus", Semicolon: "Semicolon",
  Equal: "Equal", BracketLeft: "LBracket", BracketRight: "RBracket", Quote: "Quote",
  Backquote: "Backquote", Backslash: "Backslash", NumpadDecimal: "Period",
  NumpadDivide: "Slash", NumpadSubtract: "Minus",
});
for (const c of "ABCDEFGHIJKLMNOPQRSTUVWXYZ") keyMap["Key" + c] = c;
for (const d of "0123456789") {
  keyMap["Digit" + d] = d;
  keyMap["Numpad" + d] = d;
}

// メニュー・最初の画面を操作しているときは、キーをゲストに送らない。
const inUi = () => !$("menu").hidden || [...document.querySelectorAll(".overlay")].some((o) => !o.hidden);
window.addEventListener("keydown", (e) => {
  // IME の変換中のキーは送らない（§7.1。keyCode 229 は変換中を示す古い値）
  if (e.isComposing || e.keyCode === 229 || inUi() || !booted) return;
  const k = keyMap[e.code];
  if (!k || e.metaKey) return;
  e.preventDefault();
  if (e.repeat) return; // 自動リピートはゲスト側に任せる
  press(k, "kbd:" + e.code);
});
window.addEventListener("keyup", (e) => {
  const k = keyMap[e.code];
  if (!k) return;
  if (held.get(k)?.has("kbd:" + e.code)) e.preventDefault();
  release(k, "kbd:" + e.code);
});
// フォーカスを失ったら押しっぱなしを解除する（keyup が届かなくなるため）。
window.addEventListener("blur", () => {
  for (const [key, s] of held) {
    for (const src of [...s]) if (src.startsWith("kbd:")) release(key, src);
  }
  penUp();
});

// ---- PWA（§7.5）----
// Service Worker はオフラインで開けるようにするだけ（ネットワーク優先で、つながれば
// 常に新しい版を使う。版の切り替えはページを開き直したとき = 自動保存の後）。
if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("sw.js").catch((e) => log(`Service Worker を登録できません: ${e.message}`));
}
