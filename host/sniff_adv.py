"""Listen for controller advertisements (HOME + Y on left, HOME + B on right).

Usage: sniff_adv.py [--bad-crc] [--freq N]
"""
import argparse

from bridge import ADV_BASE, ADV_FREQ, ADV_PREFIX, Bridge, Rx


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bad-crc", action="store_true", help="also show packets that fail CRC")
    ap.add_argument("--freq", type=int, default=ADV_FREQ)
    args = ap.parse_args()

    b = Bridge()
    print("dongle:", b.ping())
    b.set_address(ADV_BASE, ADV_PREFIX, args.freq, pcnf0=8)
    b.layout(0, 1, args.bad_crc)
    b.rx()
    print(f"listening on {2400 + args.freq} MHz for AA:{ADV_BASE:08X} ... hold HOME + Y (left) or HOME + B (right)")
    last = None
    for ev in b.events():
        if isinstance(ev, Rx):
            dt = "" if last is None else f"+{(ev.ts - last) / 1000:.1f}ms"
            last = ev.ts
            print(f"{ev.ts / 1e6:10.4f}s {dt:>10} crc={'ok ' if ev.crc_ok else 'BAD'} rssi=-{ev.rssi:<3} {ev.data.hex(' ')}")


if __name__ == "__main__":
    main()
