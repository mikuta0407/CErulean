#!/usr/bin/env python3
"""pcsample.py <modules.txt> [最小回数]: `cerulean run --sample N` の出力（標準入力）を
モジュール名の時系列にする（調査用。std のみ）。

modules.txt は romtoc.py の出力。ユーザーのプロセスの PC はスロットの位置
（32MB 単位）に移っているので、PC そのままと下位 25 ビットの両方で引く。
出力は「区間の最初の命令数 モジュール名 連続したサンプル数」（最小回数未満の区間は省く）。
例: cerulean run --snap-load s.snap --no-idle-skip --sample 50 --max-steps N 2>&1 |
    tools/re/pcsample.py mods.txt 2 | grep -v 'kernel\\|coredll'
"""
import re
import sys

mods = []
for line in open(sys.argv[1]):
    a, s, n = line.split()
    mods.append((int(a, 16), int(s, 16), n))
minc = int(sys.argv[2]) if len(sys.argv) > 2 else 3


def name(pc):
    if pc >= 0x80000000:
        return "kernel"
    for cand in (pc, pc & 0x1FFFFFF):
        for a, s, nm in mods:
            if a <= cand < a + s:
                return nm
    return f"?{pc:08X}"


prev = None
start = cnt = 0
for line in sys.stdin:
    m = re.search(r"(\d+)\s+sample PC=([0-9A-F]{8})", line)
    if not m:
        continue
    n, nm = int(m.group(1)), name(int(m.group(2), 16))
    if nm != prev:
        if prev and cnt >= minc:
            print(start, prev, cnt)
        prev, start, cnt = nm, n, 0
    cnt += 1
if prev:
    print(start, prev, cnt)
