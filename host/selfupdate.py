"""Reflash the dongle in-application (no bootloader, no button press).

Usage: selfupdate.py <firmware.bin>

The bootloader CRC-checks the first `image_size` bytes at 0x1000 against values in
its write-protected settings page. We patch 4 bytes at a reserved vector-table slot
(offset 0x1C) so the new image matches that stored CRC, then the running bridge
firmware stages the image in RAM and flashes it.
"""
import struct
import sys
import time
import zlib

from bridge import Bridge

FORGE_OFF = 0x1C  # reserved Cortex-M vector (unused by hardware)
SETTINGS = 0xFF000

TABLE = []
for i in range(256):
    c = i
    for _ in range(8):
        c = (c >> 1) ^ 0xEDB88320 if c & 1 else c >> 1
    TABLE.append(c)
TOP = {t >> 24: i for i, t in enumerate(TABLE)}


def _fwd(state, data):
    for b in data:
        state = (state >> 8) ^ TABLE[(state ^ b) & 0xFF]
    return state


def _back(state, data):
    for b in reversed(data):
        idx = TOP[state >> 24]
        state = (((state ^ TABLE[idx]) << 8) & 0xFFFFFFFF) | (idx ^ b)
    return state


def forge(region, pos, target):
    """Return `region` with 4 bytes at `pos` chosen so zlib.crc32(region) == target."""
    region = bytearray(region)
    s1 = _fwd(0xFFFFFFFF, region[:pos])
    s2 = _back(target ^ 0xFFFFFFFF, region[pos + 4 :])
    x = _back(s2, b"\0\0\0\0") ^ s1
    region[pos : pos + 4] = struct.pack("<I", x)
    assert zlib.crc32(bytes(region)) == target
    return bytes(region)


class UpdateError(Exception):
    pass


def self_update(fw, progress=print, bridge=None):
    """Flash `fw` (bytes) through the running dongle firmware. `progress(text)` gets status lines."""
    b = bridge or Bridge()
    version = b.ping()[1]
    progress(f"dongle: {version}")
    if "nice!nano" in version:
        raise UpdateError("this is a nice!nano: install questpad-nicenano.uf2 (uf2.py) instead")
    image_size, image_crc, bank_code = (b.peek(SETTINGS + o) for o in (0x18, 0x1C, 0x20))
    if bank_code != 1 or image_size < FORGE_OFF + 4:
        raise UpdateError(f"unexpected bootloader settings (size={image_size:#x} code={bank_code}); use DFU")
    total = max(len(fw), image_size)
    total = (total + 3) & ~3
    img = bytearray(fw.ljust(total, b"\xFF"))
    if img[FORGE_OFF : FORGE_OFF + 4] != b"\0\0\0\0":
        raise UpdateError("vector-table slot 0x1C is not free in this image")
    img[:image_size] = forge(img[:image_size], FORGE_OFF, image_crc)
    img = bytes(img)

    def call(payload):
        b.send(payload)
        f = b._wait(0xF0, timeout=3)
        if not f[2]:
            raise UpdateError(f"dongle rejected step {payload[0]:#x}")

    progress(f"uploading {len(img)} bytes")
    call(struct.pack("<BI", 0x70, len(img)))
    step = 200
    for off in range(0, len(img), step):
        call(struct.pack("<BI", 0x71, off) + img[off : off + step])
        if off % 4000 == 0:
            progress(f"uploading {off * 100 // len(img)}%")
    b.send(struct.pack("<BII", 0x72, len(img), zlib.crc32(img)))
    progress("flashing; dongle will reboot")
    try:
        b.s.close()
    except OSError:
        pass
    time.sleep(3)
    for _ in range(30):
        try:
            version = Bridge().ping()[1]
            progress(f"dongle: {version}")
            return version
        except (SystemExit, Exception):
            time.sleep(0.5)
    raise UpdateError("dongle did not come back; press its RESET button and flash via DFU")


def main():
    fw = open(sys.argv[1], "rb").read()
    try:
        self_update(fw)
    except UpdateError as e:
        sys.exit(str(e))


if __name__ == "__main__":
    main()
