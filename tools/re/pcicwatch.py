#!/usr/bin/env python3
"""pcicwatch.py <watch.log>: `--watch 0x10000000-0x17FFFFFF` のログ（標準エラーの出力）を、
PC カードコントローラのレジスタ操作（Index 0x3E0 / Data 0x3E1 を組にした「reg[XX]=値」）と
カードへのアクセスに並べ直す（調査用。std のみ）。属性メモリ（PA 0x1000xxxx）の連続した
バイト読み出しは 1 行にまとめ、同じ行の繰り返しは「xN」にする。
"""
import re
import sys

idx = 0
rows = []
for line in open(sys.argv[1]):
    m = re.search(r"(\d+)\s+watch (\w)(\d+) \S+\s+PA=(\w+) v=(\w+)", line)
    if not m:
        continue
    n, rw, sz, pa, v = m.groups()
    pa, v = int(pa, 16), int(v, 16)
    if pa == 0x110003E0 and rw == "W":
        idx = v
        continue
    if pa == 0x110003E1:
        rows.append((n, f"{rw} reg[{idx:02X}]={v:02X}"))
    else:
        rows.append((n, f"{rw}{sz} {pa:08X}={v:X}"))
out = []
i = 0
while i < len(rows):
    n, s = rows[i]
    if s.startswith("R8 1000"):
        j = i
        vals = []
        while j < len(rows) and rows[j][1].startswith("R8 1000"):
            a, v = rows[j][1][3:].split("=")
            vals.append((int(a, 16), int(v, 16)))
            j += 1
        data = bytes(v for _, v in vals[:32]).hex()
        out.append(f"{n} attr reads {vals[0][0]:08X}..{vals[-1][0]:08X} ({len(vals)}): {data}")
        i = j
        continue
    out.append(f"{n} {s}")
    i += 1
prev, cnt = None, 0
for o in out:
    k = o.split(" ", 1)[1]
    if k == prev:
        cnt += 1
        continue
    if cnt:
        print(f"    x{cnt + 1}")
    cnt = 0
    print(o)
    prev = k
if cnt:
    print(f"    x{cnt + 1}")
