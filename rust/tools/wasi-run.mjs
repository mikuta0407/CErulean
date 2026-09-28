#!/usr/bin/env node
// wasi-run.mjs <module.wasm> [args...]
// cargo の wasm32-wasip1 のテストランナー。Node の WASI で実行し、終了コードを返す。
import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";
import { argv, env, exit } from "node:process";

const [, , wasmPath, ...args] = argv;
const wasi = new WASI({ version: "preview1", args: [wasmPath, ...args], env, returnOnExit: true });
const module = await WebAssembly.compile(await readFile(wasmPath));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
exit(wasi.start(instance));
