"""Read / write controller registers over the live link (the dongle must be hosting it).

  regs.py scan [left|right]            read every register, print the replies
  regs.py read left 0x24               read one register
  regs.py write right 0x97 c8          write bytes (hex) to a register

Replies are uplink TL frames [slot][reg][flags][data] without the 0x40 notification bit;
flags 0x20 = error (register unknown / rejected).
"""
import argparse
from bridge import Bridge

TL_NOTIFICATION, TL_ERROR = 0x40, 0x20


def request(b, slot, reg, data=b"", read=True, timeout=0.5):
    # Send without waiting for the queue ack, so no reply frame is skipped while waiting.
    b.send(bytes([0x26, slot, reg, int(read)]) + bytes(data))
    for f in b.frames(timeout):
        if f[0] == 0x93 and not f[1]:
            raise RuntimeError("dongle would not queue the request (slot not connected?)")
        if f[0] == 0xA1 and f[1] == slot and f[3]:
            p = f[4:]
            if len(p) >= 3 and not p[2] & TL_NOTIFICATION and p[1] == reg:
                return bool(p[2] & TL_ERROR), p[3:]
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("action", choices=["scan", "read", "write"])
    ap.add_argument("side", nargs="?", default="left")
    ap.add_argument("reg", nargs="?", type=lambda s: int(s, 0))
    ap.add_argument("data", nargs="?", default="")
    args = ap.parse_args()

    b = Bridge()
    slots = b.link_slots()
    if args.side not in slots:
        raise SystemExit(f"{args.side} controller not connected (joined: {slots})")
    slot = slots[args.side]
    print(f"{args.side} = slot {slot}")

    if args.action == "scan":
        for reg in range(0xA0):
            r = request(b, slot, reg)
            if r is None:
                print(f"  {reg:#04x}: no reply")
            elif not r[0]:
                d = r[1]
                text = d.rstrip(b"\0")
                show = f'  "{text.decode()}"' if text and all(32 <= c < 127 for c in text) else ""
                print(f"  {reg:#04x}: [{len(d):2d}] {d.hex(' ')}{show}")
    elif args.action == "read":
        print(request(b, slot, args.reg))
    else:
        r = request(b, slot, args.reg, bytes.fromhex(args.data), read=False)
        print("reply:", r)


if __name__ == "__main__":
    main()
