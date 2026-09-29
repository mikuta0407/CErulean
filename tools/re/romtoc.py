#!/usr/bin/env python3
"""romtoc.py <image.bin> [modules|files]: B000FF イメージの ROM の TOC を表示する（調査用。std のみ）。

modules（既定）: 「vbase vsize 名前」を 1 行ずつ（pcsample.py の入力になる）。
files: ファイル（レジストリのハイブ等）の「属性 実サイズ 圧縮後 名前」。属性 0x800 は圧縮。

イメージ先頭+0x40 の 'ECEC' の次が ROMHDR の VA。ROMHDR（0x54 バイト）の後に
モジュールの TOC（32 バイト×nummods。+16 がファイル名、+20 が e32_rom で、その +8 が
vbase・+0x14 が vsize）、続いてファイルの TOC（28 バイト×numfiles）。
"""
import struct
import sys


def main():
    path = sys.argv[1]
    what = sys.argv[2] if len(sys.argv) > 2 else "modules"
    d = open(path, "rb").read()
    if d[:7] != b"B000FF\n":
        sys.exit("not a B000FF image")
    start, _length = struct.unpack_from("<II", d, 7)
    recs = []
    off = 15
    while off < len(d):
        a, n, _c = struct.unpack_from("<III", d, off)
        off += 12
        if a == 0:
            break
        recs.append((a, d[off:off + n]))
        off += n

    def rd(va, n):
        for a, b in recs:
            if a <= va and va + n <= a + len(b):
                return b[va - a:va - a + n]
        raise ValueError(f"VA {va:08X} not in image")

    def u32(va):
        return struct.unpack("<I", rd(va, 4))[0]

    def cstr(va):
        s = b""
        while (c := rd(va, 1)) != b"\0":
            s += c
            va += 1
        return s.decode("latin1")

    if rd(start + 0x40, 4) != b"ECEC":
        sys.exit("no ECEC signature")
    rh = u32(start + 0x44)
    f = struct.unpack("<21I", rd(rh, 0x54))
    nummods, numfiles = f[4], f[12]
    if what == "modules":
        for k in range(nummods):
            t = rh + 0x54 + 32 * k
            e = u32(t + 20)
            print(f"{u32(e + 8):#x} {u32(e + 0x14):#x} {cstr(u32(t + 16))}")
    else:
        base = rh + 0x54 + 32 * nummods
        for k in range(numfiles):
            attr, _t1, _t2, real, comp, name, _off = struct.unpack("<7I", rd(base + 28 * k, 28))
            print(f"{attr:#x} {real} {comp} {cstr(name)}")


main()
