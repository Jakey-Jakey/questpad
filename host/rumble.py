"""Buzz the controllers through the dongle's HID gamepad output report (what games use).

  rumble.py LEFT RIGHT [SECONDS]    amplitudes 0..255, default 0.5 s, then off
"""
import sys
import time

import hid


def main():
    left, right = int(sys.argv[1]), int(sys.argv[2])
    secs = float(sys.argv[3]) if len(sys.argv) > 3 else 0.5
    dev = hid.device()
    dev.open(0x1209, 0x0001)
    try:
        dev.write([0x00, left, right])  # report id 0 (none), then [left, right]
        time.sleep(secs)
    finally:
        dev.write([0x00, 0, 0])
        dev.close()


if __name__ == "__main__":
    main()
