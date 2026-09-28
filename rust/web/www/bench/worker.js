// 計測用の Worker（index.html から起動される。本体は bench-core.js）。
import init, * as wasm from "../pkg/cerulean_web.js";
import { runBench } from "./bench-core.js";

onmessage = async ({ data: { imageBytes, expected } }) => {
  const log = (msg) => postMessage({ log: msg });
  try {
    await init();
    wasm.installPanicHook();
    let opfs;
    try {
      opfs = await navigator.storage.getDirectory();
    } catch (e) {
      log(`OPFS unavailable: ${e}`);
    }
    const result = await runBench({ wasm, imageBytes, expected, log, opfs });
    postMessage({ result });
  } catch (e) {
    // wasm の panic は RuntimeError（trap）になる。インスタンスは以後使えない。
    postMessage({ error: `${e?.name}: ${e?.message ?? e}` });
  }
};
