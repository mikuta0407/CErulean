// 段階2 の計測の本体（計画書 §9 段階2）。ブラウザの Worker（worker.js）と Node
// （tools/web-bench.mjs）の両方から使うので、Web 標準の API だけを使う
// （crypto.subtle・CompressionStream・performance）。
//
// 1. リセットから boot-1200M（12 億命令）を走らせ、命令/秒を測り、終わりの CPU 状態・
//    RAM・UART1・画面のハッシュを基準（testdata/golden/expected/boot-1200M.jsonl の
//    stop の行）と比べる。
// 2. スナップショットの保存（無圧縮）・gzip 圧縮・展開・読み込みの時間と大きさを測り、
//    読み込んだ状態が元と同じかを確かめる（自動保存の頻度と圧縮方式の判断材料。§4.3・§7.3）。
// 3. 渡されれば OPFS への書き込み・読み出しの時間を測る（ブラウザの Worker だけ）。

const STEPS = 1_200_000_000n;
const CHUNK = 50_000_000n;

const hex = (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, "0")).join("");

// SHA-256（FIPS 180-4）の JS 実装。crypto.subtle は安全なコンテキスト（HTTPS か
// localhost）でしか使えないので、LAN の IP で開いた iPhone/iPad 等ではこちらを使う。
const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

function sha256js(bytes) {
  const b = new Uint8Array(bytes);
  const total = Math.ceil((b.length + 9) / 64) * 64;
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]);
  const w = new Uint32Array(64);
  const tail = new Uint8Array(total - Math.floor(b.length / 64) * 64);
  const full = Math.floor(b.length / 64) * 64;
  tail.set(b.subarray(full));
  tail[b.length - full] = 0x80;
  const bits = b.length * 8;
  const tv = new DataView(tail.buffer);
  tv.setUint32(tail.length - 8, Math.floor(bits / 2 ** 32));
  tv.setUint32(tail.length - 4, bits >>> 0);
  const bv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const block = (dv, off) => {
    for (let i = 0; i < 16; i++) w[i] = dv.getUint32(off + 4 * i);
    for (let i = 16; i < 64; i++) {
      const x = w[i - 15], y = w[i - 2];
      const s0 = ((x >>> 7) | (x << 25)) ^ ((x >>> 18) | (x << 14)) ^ (x >>> 3);
      const s1 = ((y >>> 17) | (y << 15)) ^ ((y >>> 19) | (y << 13)) ^ (y >>> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
    }
    let [a, bb, c, d, e, f, g, hh] = h;
    for (let i = 0; i < 64; i++) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const t1 = (hh + S1 + ((e & f) ^ (~e & g)) + K[i] + w[i]) | 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const t2 = (S0 + ((a & bb) ^ (a & c) ^ (bb & c))) | 0;
      [hh, g, f, e, d, c, bb, a] = [g, f, e, (d + t1) | 0, c, bb, a, (t1 + t2) | 0];
    }
    h[0] += a; h[1] += bb; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
  };
  for (let off = 0; off < full; off += 64) block(bv, off);
  for (let off = 0; off < tail.length; off += 64) block(tv, off);
  return Array.from(h, (x) => x.toString(16).padStart(8, "0")).join("");
}

export const sha256 = async (b) =>
  globalThis.crypto?.subtle ? hex(await crypto.subtle.digest("SHA-256", b)) : sha256js(b);

async function pipe(bytes, stream) {
  const out = new Blob([bytes]).stream().pipeThrough(stream);
  return new Uint8Array(await new Response(out).arrayBuffer());
}

/// wasm は web クレートの wasm-bindgen の出力（--target web）。imageBytes は PPC_USA.bin、
/// expected は基準の stop の行（JSON を解釈したもの）。log は進み具合の表示。
/// opfs は Worker の OPFS のディレクトリ（なければ省略）。
export async function runBench({ wasm, imageBytes, expected, log, opfs }) {
  const r = { userAgent: typeof navigator !== "undefined" ? navigator.userAgent : "node" };
  const emu = new wasm.Emu();
  emu.loadImage(imageBytes, "PPC_USA.bin", Int32Array.from([2006, 1, 2, 15, 4, 5]));
  const uart = [];
  const t0 = performance.now();
  for (let target = CHUNK; target <= STEPS; target += CHUNK) {
    const st = emu.run(target);
    uart.push(emu.takeUart());
    if (st !== "ok") throw new Error(`stopped: ${emu.stopJson()}`);
    if (target % 200_000_000n === 0n) {
      const el = (performance.now() - t0) / 1000;
      log(`${target / 1_000_000n}M steps, ${(Number(target) / el / 1e6).toFixed(1)}M steps/s`);
    }
  }
  const el = (performance.now() - t0) / 1000;
  r.bootSeconds = el;
  r.mips = Number(STEPS) / el / 1e6;
  r.codePages = emu.codePages();

  const dump = emu.cpuDump();
  const screen = emu.frame();
  const uartAll = new Uint8Array(uart.reduce((n, b) => n + b.length, 0));
  uart.reduce((o, b) => (uartAll.set(b, o), o + b.length), 0);
  r.got = {
    cpu_sha256: await sha256(dump),
    ram_sha256: await sha256(emu.ram()),
    uart1_sha256: await sha256(uartAll),
    screen_sha256: screen.length ? await sha256(screen) : "",
  };
  r.match = Object.keys(r.got).every((k) => r.got[k] === expected[k]);
  log(`boot-1200M: ${r.mips.toFixed(1)}M steps/s, match=${r.match}`);

  // スナップショット（無圧縮 → gzip → 展開 → 読み込み）。
  let t = performance.now();
  const snap = emu.saveSnapshot("bench");
  r.snapSaveMs = performance.now() - t;
  r.snapBytes = snap.length;
  t = performance.now();
  const gz = await pipe(snap, new CompressionStream("gzip"));
  r.gzipMs = performance.now() - t;
  r.gzipBytes = gz.length;
  t = performance.now();
  const back = await pipe(gz, new DecompressionStream("gzip"));
  r.gunzipMs = performance.now() - t;
  t = performance.now();
  const emu2 = new wasm.Emu();
  emu2.loadSnapshot(back);
  r.snapLoadMs = performance.now() - t;
  r.snapRoundTrip = (await sha256(emu2.cpuDump())) === r.got.cpu_sha256 && (await sha256(emu2.ram())) === r.got.ram_sha256;
  emu2.free();
  log(`snapshot ${(r.snapBytes / 1e6).toFixed(1)}MB save ${r.snapSaveMs.toFixed(0)}ms, gzip ${(r.gzipBytes / 1e6).toFixed(1)}MB ${r.gzipMs.toFixed(0)}ms, gunzip ${r.gunzipMs.toFixed(0)}ms, load ${r.snapLoadMs.toFixed(0)}ms, ok=${r.snapRoundTrip}`);

  if (opfs) {
    // OPFS の同期アクセスハンドル（Worker だけで使える）への書き込み・読み出し。
    const fh = await opfs.getFileHandle("bench-snapshot.gz", { create: true });
    const h = await fh.createSyncAccessHandle();
    t = performance.now();
    h.truncate(0);
    h.write(gz, { at: 0 });
    h.flush();
    r.opfsWriteMs = performance.now() - t;
    t = performance.now();
    const rb = new Uint8Array(h.getSize());
    h.read(rb, { at: 0 });
    r.opfsReadMs = performance.now() - t;
    h.close();
    await opfs.removeEntry("bench-snapshot.gz");
    log(`OPFS write ${r.opfsWriteMs.toFixed(0)}ms, read ${r.opfsReadMs.toFixed(0)}ms`);
  }
  emu.free();
  return r;
}

// ---- JIT 診断（iPhone だけ遅い原因の切り分け。2026-09-29）----
//
// 同じ Worker で 3 つを測り、どれが遅いかで原因を見分ける:
//   - 素の JS のループ: これも遅ければ、端末の設定（ロックダウンモード等）で JIT が
//     全体に無効になっている。
//   - 小さな wasm 関数のループ: JS は速くこれだけ遅ければ、wasm の JIT が無効。
//   - エミュレータ本体（合成プログラムをアイドルスキップなしで回す）: 上の 2 つが速く
//     これだけ遅ければ、巨大な実行ループ関数が最適化の対象から外れている等、
//     エミュレータの関数に固有の原因。

/// (param i32) (result i32): n から 1 まで数えながら足し上げる。手で組んだ wasm
/// （local.get/i32.add/local.set/i32.sub/local.tee/br_if のループ）。
const TINY_WASM = Uint8Array.from([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0, 0, 0, 0x01, 0x06, 0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00,
  0x07, 0x05, 0x01, 0x01, 0x66, 0x00, 0x00, 0x0a, 0x1b, 0x01, 0x19, 0x01, 0x01, 0x7f, 0x03, 0x40, 0x20, 0x01,
  0x20, 0x00, 0x6a, 0x21, 0x01, 0x20, 0x00, 0x41, 0x01, 0x6b, 0x22, 0x00, 0x0d, 0x00, 0x0b, 0x20, 0x01, 0x0b,
]);

export async function runJitProbe({ wasm, synthetic, log }) {
  const r = { userAgent: typeof navigator !== "undefined" ? navigator.userAgent : "node" };
  const N = 300_000_000;
  let t = performance.now();
  let sum = 0;
  for (let i = N; i > 0; i--) sum = (sum + i) | 0;
  r.jsLoopMs = performance.now() - t;
  r.jsSum = sum;
  log(`JS loop ${N / 1e6}M: ${r.jsLoopMs.toFixed(0)}ms`);

  const { instance } = await WebAssembly.instantiate(TINY_WASM);
  t = performance.now();
  r.wasmSum = instance.exports.f(N);
  r.wasmLoopMs = performance.now() - t;
  log(`tiny wasm loop ${N / 1e6}M: ${r.wasmLoopMs.toFixed(0)}ms`);

  // エミュレータ本体: タイマー割り込みを待つ合成プログラムを、アイドルスキップなしで
  // 2000 万命令回す（全命令を実際に実行させる）。
  const emu = new wasm.Emu();
  emu.loadImage(synthetic, "idle.words", Int32Array.from([2006, 1, 2, 15, 4, 5]));
  emu.setIdleSkip(false);
  t = performance.now();
  emu.run(20_000_000n);
  r.emuMs = performance.now() - t;
  r.emuMips = 20 / (r.emuMs / 1000);
  emu.free();
  log(`emulator (synthetic, no idle skip) 20M: ${r.emuMs.toFixed(0)}ms (${r.emuMips.toFixed(1)}M steps/s)`);
  return r;
}
