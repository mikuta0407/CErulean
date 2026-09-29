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
assert.throws(() => emu.input("keydown", 0, 0, "NoSuchKey"));
assert.throws(() => emu.input("down", 240, 0, ""));
assert.throws(() => emu.input("bogus", 0, 0, ""));
const script = emu.recordStop("start.snap", "abc");
assert.equal(emu.recording(), false);
assert.match(script, /@1000i down 10 20\n@2000i up\n@2000i key down Enter\n/);
emu.free();
console.log("node-smoke: ok");
