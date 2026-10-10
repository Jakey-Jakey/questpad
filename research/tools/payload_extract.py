"""Minimal Android A/B OTA payload.bin extractor (full OTAs only).

Usage: payload_extract.py <payload.bin|ota.zip> <out_dir> [partition ...]
With no partitions given, lists partitions and exits.
"""
import bz2
import lzma
import struct
import sys
import zipfile
from pathlib import Path


def varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, i
        shift += 7


def fields(buf):
    """Yield (field_number, wire_type, value) for a protobuf message."""
    i = 0
    while i < len(buf):
        key, i = varint(buf, i)
        num, wt = key >> 3, key & 7
        if wt == 0:
            val, i = varint(buf, i)
        elif wt == 1:
            val = buf[i:i + 8]
            i += 8
        elif wt == 2:
            ln, i = varint(buf, i)
            val = buf[i:i + ln]
            i += ln
        elif wt == 5:
            val = buf[i:i + 4]
            i += 4
        else:
            raise ValueError(f"unsupported wire type {wt}")
        yield num, wt, val


def parse_extent(buf):
    d = {1: 0, 2: 0}
    for n, _, v in fields(buf):
        d[n] = v
    return d[1], d[2]


def parse_op(buf):
    op = {"type": 0, "offset": 0, "length": 0, "dst": []}
    for n, _, v in fields(buf):
        if n == 1:
            op["type"] = v
        elif n == 2:
            op["offset"] = v
        elif n == 3:
            op["length"] = v
        elif n == 6:
            op["dst"].append(parse_extent(v))
    return op


def parse_partition(buf):
    p = {"name": "", "ops": [], "size": 0}
    for n, _, v in fields(buf):
        if n == 1:
            p["name"] = v.decode()
        elif n == 8:
            p["ops"].append(parse_op(v))
        elif n == 7:
            for n2, _, v2 in fields(v):
                if n2 == 1:
                    p["size"] = v2
    return p


def open_payload(path):
    if path.endswith(".zip"):
        z = zipfile.ZipFile(path)
        info = z.getinfo("payload.bin")
        if info.compress_type != zipfile.ZIP_STORED:
            raise SystemExit("payload.bin is compressed in zip; unzip it first")
        f = open(path, "rb")
        f.seek(info.header_offset)
        hdr = f.read(30)
        name_len, extra_len = struct.unpack("<HH", hdr[26:30])
        return f, info.header_offset + 30 + name_len + extra_len
    return open(path, "rb"), 0


def main():
    src, out = sys.argv[1], Path(sys.argv[2])
    wanted = set(sys.argv[3:])
    f, base = open_payload(src)
    f.seek(base)
    magic, version, manifest_size = struct.unpack(">4sQQ", f.read(20))
    assert magic == b"CrAU", magic
    sig_size = struct.unpack(">I", f.read(4))[0] if version >= 2 else 0
    manifest = f.read(manifest_size)
    data_base = base + 20 + (4 if version >= 2 else 0) + manifest_size + sig_size

    block_size = 4096
    parts = []
    for n, _, v in fields(manifest):
        if n == 3:
            block_size = v
        elif n == 13:
            parts.append(parse_partition(v))

    for p in parts:
        print(f"{p['name']:24s} {p['size']:>12d} bytes  {len(p['ops'])} ops")
    if not wanted:
        return

    out.mkdir(parents=True, exist_ok=True)
    for p in parts:
        if p["name"] not in wanted:
            continue
        dst = open(out / f"{p['name']}.img", "wb")
        dst.truncate(p["size"])
        for op in p["ops"]:
            f.seek(data_base + op["offset"])
            data = f.read(op["length"])
            t = op["type"]
            if t == 0:
                pass
            elif t == 1:
                data = bz2.decompress(data)
            elif t == 8:
                data = lzma.decompress(data)
            elif t in (6, 7):
                data = None
            else:
                raise SystemExit(f"op type {t} not supported (delta OTA?)")
            pos = 0
            for start, num in op["dst"]:
                ln = num * block_size
                dst.seek(start * block_size)
                dst.write(b"\0" * ln if data is None else data[pos:pos + ln])
                pos += ln
        dst.close()
        print(f"wrote {out / (p['name'] + '.img')}")


if __name__ == "__main__":
    main()
