"""Drive the dongle's HID gamepad from the serial bridge and read it back via HID."""
import struct
import time

import hid

from bridge import Bridge


def report(buttons=0, hat=8, lt=0, rt=0, lx=0, ly=0, rx=0, ry=0):
    return struct.pack("<HBBBhhhh", buttons, hat, lt, rt, lx, ly, rx, ry)


def main():
    b = Bridge()
    dev = hid.device()
    dev.open(0x1209, 0x0001)
    dev.set_nonblocking(False)
    cases = [
        ("neutral", report()),
        ("A + RB", report(buttons=0b100001)),
        ("hat right, LT half", report(hat=2, lt=128)),
        ("left stick up-right", report(lx=32767, ly=-32768)),
        ("right stick + RT", report(rx=-12345, ry=23456, rt=255)),
    ]
    ok = True
    for name, r in cases:
        b.send(b"\x30" + r)
        got = bytes(dev.read(64, 500))
        match = got == r
        ok &= match
        print(f"{name:22s} sent {r.hex()} got {got.hex()} {'OK' if match else 'MISMATCH'}")
    b.send(b"\x30" + report())
    print("ALL OK" if ok else "FAILED")


if __name__ == "__main__":
    main()
