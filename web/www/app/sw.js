// Service Worker（計画書 §7.5）: オフラインで開けるように、アプリの静的ファイル
// （HTML・JS・CSS・wasm）を控えておく。ネットワーク優先で、つながれば常に新しい版を
// 取り（控えも更新する）、つながらないときだけ控えを返す。版の切り替えはページを
// 開き直したときだけ起きるので、動作中のエミュレータが途中で別の版に変わることはない
// （開き直す前に pagehide・visibilitychange で自動保存している）。
// イメージ・保存は OPFS にあり、ここでは扱わない。
const CACHE = "cerulean-app-v3";
const SHELL = [
  "./", "index.html", "app.js", "worker.js", "i18n.js", "style.css", "manifest.webmanifest",
  "icon.svg", "icon-192.png", "icon-512.png",
  "../pkg/assets.json",
];

self.addEventListener("install", (e) => {
  e.waitUntil(
    fetch("../pkg/assets.json", { cache: "no-store" })
      .then((r) => {
        if (!r.ok) throw new Error(`asset list: ${r.status}`);
        return r.json();
      })
      .then((files) => caches.open(CACHE).then((c) => c.addAll([
        ...SHELL, ...files.map((name) => `../pkg/${name}`),
      ])))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith("cerulean-app-") && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET" || new URL(req.url).origin !== location.origin) return;
  e.respondWith(
    fetch(req)
      .then((res) => {
        if (res.ok) {
          const copy = res.clone();
          caches.open(CACHE).then((c) => c.put(req, copy));
        }
        return res;
      })
      .catch(() => caches.match(req, { ignoreSearch: true }).then((r) => r ?? Response.error())),
  );
});
