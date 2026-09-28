// node-smoke.mjs <wasm-bindgen の --target nodejs の出力ディレクトリ>
// web クレートの wasm を Node で読み込み、API が呼べることを確かめる。
import { createRequire } from "node:module";
import { resolve } from "node:path";
import assert from "node:assert/strict";

const require = createRequire(import.meta.url);
const web = require(resolve(process.argv[2], "cerulean_web.js"));
assert.equal(web.instructionsPerSecond(), 135_200_000n);
console.log("node-smoke: ok");
