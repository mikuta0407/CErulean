// 計測用の Worker（index.html から起動される。本体は bench-core.js）。
import init, * as wasm from "../pkg/cerulean_web.js";
import { runBench, runJitProbe } from "./bench-core.js";

onmessage = async ({ data: { imageBytes, expected, probe, synthetic, jit, snapshot } }) => {
  const log = (msg) => postMessage({ log: msg });
  try {
    await init();
    wasm.installPanicHook();
    if (probe) {
      postMessage({ result: await runJitProbe({ wasm, synthetic, log }) });
      return;
    }
    let opfs;
    if (snapshot) {
      try {
        opfs = await navigator.storage.getDirectory();
      } catch (e) {
        log(`OPFS unavailable: ${e}`);
      }
    }
    const result = await runBench({ wasm, imageBytes, expected, log, opfs, jit, snapshot });
    postMessage({ result });
  } catch (e) {
    // wasm の panic は RuntimeError（trap）になる。インスタンスは以後使えない。
    postMessage({ error: `${e?.name}: ${e?.message ?? e}` });
  }
};
