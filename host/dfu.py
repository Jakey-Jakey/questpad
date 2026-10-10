"""Flash the nRF52840 Dongle (PCA10059) through Nordic's Open USB DFU bootloader.

Usage: dfu.py <firmware.bin> [--port /dev/cu.usbmodemXXXX]

The dongle's Open USB DFU bootloader only enters DFU via the RESET button (the
sideways one; the red LED pulses). After the first flash, use firmware/flash.sh,
which updates in-application without the button.
"""
import argparse
import glob
import hashlib
import struct
import sys
import time
import zlib

import serial

from bridge import DONGLE_IDS

APP_ADDR = 0x1000


# --- minimal protobuf encoding for the DFU init packet (dfu-cc.proto) ---
def _varint(n):
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def _field(num, wt, payload):
    key = _varint((num << 3) | wt)
    if wt == 0:
        return key + _varint(payload)
    return key + _varint(len(payload)) + payload


def init_packet(fw):
    digest = hashlib.sha256(fw).digest()[::-1]  # nrfutil stores the hash little-endian
    hash_msg = _field(1, 0, 3) + _field(2, 2, digest)  # SHA256
    boot_val = _field(1, 0, 0) + _field(2, 2, b"")  # NO_VALIDATION
    init = (
        _field(1, 0, int(time.time()) // 60)  # fw_version: always increasing
        + _field(2, 0, 52)  # hw_version
        + _field(3, 2, _varint(0x00))  # sd_req = [0x00] (no SoftDevice)
        + _field(4, 0, 0)  # type = APPLICATION
        + _field(5, 0, 0)
        + _field(6, 0, 0)
        + _field(7, 0, len(fw))
        + _field(8, 2, hash_msg)
        + _field(9, 0, 0)  # is_debug = false
        + _field(10, 2, boot_val)
    )
    command = _field(1, 0, 1) + _field(2, 2, init)  # op_code = INIT
    return _field(1, 2, command)  # Packet.command


# --- SLIP ---
def slip(data):
    out = bytearray()
    for b in data:
        if b == 0xC0:
            out += b"\xDB\xDC"
        elif b == 0xDB:
            out += b"\xDB\xDD"
        else:
            out.append(b)
    return bytes(out) + b"\xC0"


class Dfu:
    def __init__(self, port, progress=print):
        self.progress = progress
        self.s = serial.Serial(port, 115200, timeout=2)
        self.buf = bytearray()

    def _read_frame(self):
        deadline = time.time() + 5
        while time.time() < deadline:
            b = self.s.read(1)
            if not b:
                continue
            if b == b"\xC0":
                frame, self.buf = bytes(self.buf), bytearray()
                return frame.replace(b"\xDB\xDC", b"\xC0").replace(b"\xDB\xDD", b"\xDB")
            self.buf += b
        raise TimeoutError("no DFU response")

    def req(self, op, payload=b"", reply=True):
        self.s.write(slip(bytes([op]) + payload))
        if not reply:
            return b""
        while True:
            f = self._read_frame()
            if len(f) >= 3 and f[0] == 0x60 and f[1] == op:
                if f[2] != 0x01:
                    raise RuntimeError(f"DFU op {op:#x} failed: result {f[2:].hex()}")
                return f[3:]

    def send_object(self, obj_type, data, mtu):
        max_size, _, _ = struct.unpack("<III", self.req(0x06, bytes([obj_type])))
        chunk = (mtu - 1) // 2 - 1
        crc = 0
        for off in range(0, len(data), max_size):
            obj = data[off : off + max_size]
            self.req(0x01, struct.pack("<BI", obj_type, len(obj)))
            for i in range(0, len(obj), chunk):
                self.req(0x08, obj[i : i + chunk], reply=False)
            crc = zlib.crc32(obj, crc)
            got_off, got_crc = struct.unpack("<II", self.req(0x03))
            if got_off != off + len(obj) or got_crc != crc & 0xFFFFFFFF:
                raise RuntimeError(f"CRC mismatch at {got_off:#x}: {got_crc:#x} != {crc & 0xFFFFFFFF:#x}")
            self.req(0x04)
            self.progress(f"{off + len(obj)}/{len(data)} bytes")

    def flash(self, fw):
        self.req(0x02, struct.pack("<H", 0))  # receipt notifications off
        (mtu,) = struct.unpack("<H", self.req(0x07))
        self.progress(f"bootloader MTU {mtu}; sending init packet")
        self.send_object(1, init_packet(fw), mtu)
        self.progress("sending firmware")
        self.send_object(2, fw, mtu)


def find_port(want_bootloader):
    import serial.tools.list_ports as lp

    for p in lp.comports():
        if p.vid == 0x1915 and p.pid == 0x521F and want_bootloader:
            return p.device
        if (p.vid, p.pid) in DONGLE_IDS and not want_bootloader:
            return p.device
    return None


class DfuError(Exception):
    pass


def dfu_flash(fw, progress=print, port=None):
    """Flash `fw` through the Nordic bootloader (the dongle must be in DFU: RESET button)."""
    port = port or find_port(True)
    if not port:
        raise DfuError("No DFU bootloader found. Press the dongle's RESET button (the small sideways one) "
                       "so the red LED pulses, then retry.")
    time.sleep(0.5)
    progress(f"flashing {len(fw)} bytes via {port}")
    Dfu(port, progress).flash(fw)
    progress("done; dongle is rebooting into the new firmware")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("--port")
    args = ap.parse_args()
    fw = open(args.bin, "rb").read()

    port = args.port or find_port(True)
    if not port:
        bridge = find_port(False)
        if bridge:
            print(f"asking bridge firmware on {bridge} to enter DFU")
            s = serial.Serial(bridge, 115200, timeout=0.2)
            s.write(b"\x00\x02\x7F\x00")  # (resync) + COBS frame for [0x7F]
            s.flush()
            time.sleep(0.3)
            try:
                s.close()
            except OSError:
                pass  # device already gone
        for _ in range(50):
            time.sleep(0.2)
            port = find_port(True)
            if port:
                break
    try:
        dfu_flash(fw, port=port)
    except DfuError as e:
        sys.exit(str(e))


if __name__ == "__main__":
    main()
