// node-smoke.mjs <wasm-bindgen の --target nodejs の出力ディレクトリ>
// web クレートの wasm を Node で読み込み、API が呼べることを確かめる。
import { createRequire } from "node:module";
import { resolve } from "node:path";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const require = createRequire(import.meta.url);
const web = require(resolve(process.argv[2], "cerulean_web.js"));
assert.equal(web.instructionsPerSecond(), 135_200_000n);

// 対話入力と記録（ブラウザ版の Worker が使う）: 合成プログラムを少し進め、入力を
// 記録してスクリプトに書けること。
const words = readFileSync(new URL("../../../testdata/golden/synthetic/idle.words", import.meta.url));
const emu = new web.Emu();
emu.loadImage(words, "idle.words", Int32Array.from([2006, 1, 2, 15, 4, 5]));
assert.equal(emu.run(1000n), "ok");
emu.recordStart();
assert.equal(emu.recording(), true);
emu.input("down", 10, 20, "");
assert.equal(emu.run(2000n), "ok");
emu.input("up", 0, 0, "");
emu.input("keydown", 0, 0, "Enter");
emu.setClock(Int32Array.from([2030, 1, 2, 3, 4, 5]));
assert.throws(() => emu.setClock(Int32Array.from([2030, 1, 2])));
assert.throws(() => emu.input("keydown", 0, 0, "NoSuchKey"));
assert.throws(() => emu.input("down", 240, 0, ""));
assert.throws(() => emu.input("bogus", 0, 0, ""));
const script = emu.recordStop("start.snap", "abc");
assert.equal(emu.recording(), false);
assert.match(script, /@1000i down 10 20\n@2000i up\n@2000i key down Enter\n@2000i rtc 2030-01-02T03:04:05\n/);

// スナップショットの小分けの読み書き: 1MB 以下の断片で書き、断片のまま読み戻すと
// 同じ状態になる（Worker の自動保存・再開が使う）。
const pieces = [];
emu.saveSnapshotTo("img", (b) => pieces.push(b));
assert.ok(pieces.length > 100 && pieces.every((b) => b.length <= 1 << 20));
const all = Buffer.concat(pieces);
assert.deepEqual(all, Buffer.from(emu.saveSnapshot("img")));
const emu2 = new web.Emu();
let off = 0;
const id = emu2.loadSnapshotFrom((buf) => {
  const n = Math.min(buf.length, all.length - off);
  buf.set(all.subarray(off, off + n));
  off += n;
  return n;
});
assert.equal(id, "img");
assert.equal(emu2.steps(), emu.steps());
assert.deepEqual(emu2.cpuDump(), emu.cpuDump());
assert.notEqual(emu2.ramPtr(), 0);
// 切り詰めたものはエラー（パニックしない）
const emu3 = new web.Emu();
let off3 = 0;
assert.throws(() =>
  emu3.loadSnapshotFrom((buf) => {
    const n = Math.min(buf.length, all.length - 1000 - off3);
    buf.set(all.subarray(off3, off3 + n));
    off3 += n;
    return n;
  }),
);
for (const e of [emu, emu2, emu3]) e.free();
console.log("node-smoke: ok");
