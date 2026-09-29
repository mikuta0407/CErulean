#!/usr/bin/env python3
"""serve-bench.py [port]: rust/web/www（計測ページ bench・ブラウザ版 app）を配信する。

計測ページと期待値・合成プログラムだけを並べた tmp/bench-site を作って配信する
（リポジトリ全体を配信すると tmp/images のイメージまで見えてしまうため）。
ブラウザが古い worker.js や wasm を使い回さないよう、Cache-Control: no-store を付ける
（Python の http.server は付けないので、Safari が前の版を使って失敗したことがある）。
"""
import http.server
import os
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
site = os.path.join(root, "tmp", "bench-site")
for link, target in [
    ("rust/web/www", "rust/web/www"),
    ("testdata/golden/expected", "testdata/golden/expected"),
    ("testdata/golden/synthetic", "testdata/golden/synthetic"),
]:
    path = os.path.join(site, link)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if not os.path.islink(path):
        os.symlink(os.path.join(root, target), path)


class NoCache(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
os.chdir(site)
print(f"serving {site} on :{port} (open /rust/web/www/app/ or /rust/web/www/bench/)")
http.server.ThreadingHTTPServer(("", port), NoCache).serve_forever()
