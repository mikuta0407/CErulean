#!/usr/bin/env python3
# MSI 内の CAB から指定したファイルを抽出する（Python 標準ライブラリのみ）。
# 対応: OLE CFB、CAB の無圧縮 / MSZIP。インストーラーは実行しない。
import argparse
import struct
import os
import zlib
from pathlib import Path

def cfb_streams(path):
    d = open(path, 'rb').read()
    if d[:8] != bytes.fromhex('d0cf11e0a1b11ae1'):
        raise ValueError('not an MSI / OLE CFB file')
    ssz = 1 << struct.unpack_from('<H', d, 0x1E)[0]
    mssz = 1 << struct.unpack_from('<H', d, 0x20)[0]
    nfat, dirstart, _, cutoff, minifatstart, nminifat, difatstart, ndifat = struct.unpack_from('<IIIIIIII', d, 0x2C)
    sec = lambda n: d[(n + 1) * ssz:(n + 2) * ssz]
    difat = list(struct.unpack_from('<109I', d, 0x4C))
    s = difatstart
    for _ in range(ndifat):
        b = sec(s); v = struct.unpack('<%dI' % (ssz // 4), b)
        difat += v[:-1]; s = v[-1]
    fat = []
    for n in difat[:nfat]:
        fat += struct.unpack('<%dI' % (ssz // 4), sec(n))
    def chain(n):
        out = []
        seen = set()
        while n < 0xFFFFFFFA:
            if n in seen or n >= len(fat):
                raise ValueError("invalid CFB sector chain")
            seen.add(n)
            out.append(n); n = fat[n]
        return out
    def read(n, size=None):
        b = b''.join(sec(x) for x in chain(n))
        return b if size is None else b[:size]
    dirs = read(dirstart)
    ents = []
    for i in range(len(dirs) // 128):
        e = dirs[i * 128:(i + 1) * 128]
        nl = struct.unpack_from('<H', e, 64)[0]
        name = e[:max(nl - 2, 0)].decode('utf-16le', 'replace')
        typ = e[66]; start, size = struct.unpack_from('<IQ', e, 116)
        ents.append((name, typ, start, size & 0xFFFFFFFF))
    root = ents[0]
    ministream = read(root[2]) if root[2] < 0xFFFFFFFA else b''
    minifat = []
    if nminifat:
        mf = read(minifatstart)
        minifat = list(struct.unpack('<%dI' % (len(mf) // 4), mf))
    def mread(n, size):
        out = []
        seen = set()
        while n < 0xFFFFFFFA:
            if n in seen or n >= len(minifat):
                raise ValueError("invalid CFB mini-sector chain")
            seen.add(n)
            out.append(ministream[n * mssz:(n + 1) * mssz]); n = minifat[n]
        return b''.join(out)[:size]
    for name, typ, start, size in ents[1:]:
        if typ != 2: continue
        yield name, (mread(start, size) if size < cutoff else read(start, size))

def cab_extract(b, outdir, names):
    if b[:4] != b'MSCF':
        raise ValueError('not a CAB file')
    cbCabinet, coffFiles = struct.unpack_from('<I4xI', b, 8)
    nfolders, nfiles, flags = struct.unpack_from('<HHH', b, 26)
    if flags & 3:
        raise ValueError("multi-cabinet archives are unsupported")
    off = 36
    cbres = [0, 0, 0]
    if flags & 4:
        h, f, dres = struct.unpack_from('<HBB', b, off); off += 4 + h; cbres = [h, f, dres]
    folders = []
    for _ in range(nfolders):
        coff, ndata, comp = struct.unpack_from('<IHH', b, off); off += 8 + cbres[1]
        folders.append((coff, ndata, comp))
    data = []
    for coff, ndata, comp in folders:
        p = coff; parts = []; zd = b''
        for _ in range(ndata):
            csum, cb, ucb = struct.unpack_from('<IHH', b, p); p += 8 + cbres[2]
            blk = b[p:p + cb]; p += cb
            t = comp & 0xF
            if t == 0: u = blk
            elif t == 1:
                if blk[:2] != b'CK':
                    raise ValueError('invalid MSZIP block')
                z = zlib.decompressobj(-15, zdict=zd) if zd else zlib.decompressobj(-15)
                u = z.decompress(blk[2:]) + z.flush()
            else: raise ValueError('unsupported compression %x' % comp)
            if len(u) != ucb:
                raise ValueError('invalid CAB block length')
            parts.append(u); zd = (zd + u)[-32768:]
        data.append(b''.join(parts))
    p = coffFiles
    found = set()
    for _ in range(nfiles):
        cb, uoff, ifold = struct.unpack_from('<IIH', b, p); p += 16
        e = b.index(b'\0', p); name = b[p:e].decode('latin1'); p = e + 1
        if name not in names:
            continue
        # CAB の名前をパスとして扱わず、既存ファイルも上書きしない。
        if "/" in name or "\\" in name or ":" in name or name in (".", ".."):
            raise ValueError("unsafe CAB filename")
        if ifold >= len(data) or uoff + cb > len(data[ifold]):
            raise ValueError("invalid CAB file extent")
        os.makedirs(outdir, exist_ok=True)
        with (Path(outdir) / name).open('xb') as f:
            f.write(data[ifold][uoff:uoff + cb])
        found.add(name)
        print('  ', name, cb)
    return found

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description="Extract selected CAB members from an MSI (stored/MSZIP only)")
    parser.add_argument("msi")
    parser.add_argument("out")
    parser.add_argument("names", nargs="+", help="exact internal CAB filenames")
    args = parser.parse_args()
    try:
        found = set()
        for name, b in cfb_streams(args.msi):
            if b[:4] == b'MSCF':
                print('CAB stream', repr(name), len(b))
                found.update(cab_extract(b, args.out, set(args.names)))
        missing = set(args.names) - found
        if missing:
            raise ValueError("CAB members not found: " + ", ".join(sorted(missing)))
    except (AssertionError, ValueError, IndexError, struct.error, OSError, zlib.error) as e:
        parser.exit(1, f"extraction failed: {e}\n")
