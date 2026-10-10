"""Monitor the dongle's Pulsar link to the paired controllers (power them on normally).

The dongle hosts the link and drives its HID gamepad by itself from the pairing saved in its
flash; this tool saves `pairing.json` there if needed, then just watches.

  link.py              print joins, link stats and decoded notifications
  link.py --raw        also dump every uplink payload
  link.py --log        print the controllers' debug log (tunnelled in each uplink)
  link.py --watch      live panel of every decoded input (redraws in place)
  link.py --changes    print every change of the non-IMU notifications (for mapping inputs)
  link.py --restart    restart the link (controllers rejoin)
"""
import argparse
import json
import os
import struct
import time

from bridge import Bridge
from mapping import LOG_ITEM_LEN, Hand, combine, parse_notifications

PAIRING_FILE = os.path.join(os.path.dirname(__file__), "pairing.json")
TYPE_NAMES = {0x13001101: "left", 0x13011101: "right"}
TL_NOTIFICATION = 0x40


class Host:
    def __init__(self, raw, log, changes=False):
        self.raw, self.log, self.changes = raw, log, changes
        self.last = {}
        self.stats, self.quiet = None, False
        self.t0 = time.time()
        self.hands = {}  # slot -> (name, Hand)
        self.logbuf = {}  # slot -> partial log line
        self.seen_ids = set()

    def handle(self, f):
        kind = f[0]
        if kind == 0xA0:
            slot, dev_id, dev_type = f[1], f[2:10], struct.unpack_from("<I", f, 10)[0]
            name = TYPE_NAMES.get(dev_type, f"type {dev_type:#010x}")
            self.hands[slot] = (name, Hand())
            print(f"JOIN slot {slot}: {name} id={dev_id.hex()}")
        elif kind == 0xA2:
            name = self.hands.pop(f[1], ("?",))[0]
            print(f"DROP slot {f[1]} ({name})")
        elif kind == 0xA1:
            slot, rssi, mic_ok, payload = f[1], f[2], f[3], f[4:]
            if not mic_ok:
                print(f"  slot {slot} rssi=-{rssi} MIC FAIL raw={payload.hex()}")
                return
            if self.raw:
                print(f"  slot {slot} rssi=-{rssi} {payload.hex(' ')}")
            if slot == 0 or len(payload) < 3 or slot not in self.hands:
                return
            # [slot][tl id][tl flags][log item 4][notification entries...]
            tl_id, tl_flags = payload[1], payload[2]
            if tl_flags & TL_NOTIFICATION and len(payload) >= 3 + LOG_ITEM_LEN:
                name, hand = self.hands[slot]
                self.log_item(slot, name, payload[3 : 3 + LOG_ITEM_LEN])
                ntf = payload[3 + LOG_ITEM_LEN :]
                hand.update(ntf)
                if self.changes:
                    snap = {k: v.hex() for k, v in sorted(hand.raw.items()) if k != 1}
                    if snap != self.last.get(slot):
                        self.last[slot] = snap
                        print(f"  {time.time() - self.t0:7.2f} {name}: {snap}")
                new = {n for n, *_ in parse_notifications(ntf)} - self.seen_ids
                if new:
                    self.seen_ids.update(new)
                    print(f"  {name}: first notification ids {sorted(new)} (tl id {tl_id:#x})")
        elif kind == 0xA3:
            st = Bridge.parse_link_stats(f)
            self.stats = st
            if self.quiet:
                return
            print("stats:", " ".join(f"{k}={v}" for k, v in st.items()))
            for name, h in self.hands.values():
                print(f"  {name:5s} buttons={h.buttons:#04x} touch={h.touch:#04x} stick={h.stick} trigger={h.trigger}"
                      f" grip={h.grip} batt={h.battery}"
                      f" accel={h.accel} gyro={h.gyro}")

    def log_item(self, slot, name, item):
        """Log items: [tag][3 text bytes]; all-zero = no log pending."""
        if not self.log or not any(item):
            return
        line = self.logbuf.get(slot, b"") + item[1:].rstrip(b"\0")
        *done, line = line.split(b"\n")
        for l in done:
            # Lines start with 1-2 binary header bytes, then "<ms> [level] {module}: text".
            text = l.decode(errors="replace")
            start = text.find(" ") + 1 if text[:1] and not text[:1].isdigit() else 0
            print(f"  [{name} log] {text[start:].strip()}")
        self.logbuf[slot] = line

    def report(self):
        by_name = {name: h for name, h in self.hands.values()}
        return combine(by_name.get("left", Hand()), by_name.get("right", Hand()))


BUTTON_NAMES = ["X/A", "Y/B", "stick", "menu", "b4", "b5", "b6", "b7"]


def bar(v, lo, hi, width=21):
    """Horizontal gauge; centred marker for signed ranges."""
    f = (min(max(v, lo), hi) - lo) / (hi - lo)
    pos = round(f * (width - 1))
    if lo < 0:
        return "".join("|" if i == pos else ("+" if i == width // 2 else "-") for i in range(width))
    return "#" * pos + "." * (width - 1 - pos)


def watch_panel(host, stats):
    lines = ["\x1b[H\x1b[2J" + "Quest Touch live inputs (Ctrl-C to quit)", ""]
    if not host.hands:
        lines.append("waiting for a controller to join... (press a button to wake it)")
    for slot, (name, h) in sorted(host.hands.items()):
        btns = " ".join(n if h.buttons >> i & 1 else "." * len(n) for i, n in enumerate(BUTTON_NAMES))
        lines += [
            f"[{name} slot {slot}]  battery {h.battery}%",
            f"  buttons  {btns}   (raw {h.buttons:#04x})",
            f"  touch    {h.touch:08b}",
            f"  stick x  {bar(h.stick[0], -32768, 32767)} {h.stick[0]:+6d}",
            f"  stick y  {bar(h.stick[1], -32768, 32767)} {h.stick[1]:+6d}",
            f"  trigger  {bar(h.trigger, 0, 4095)} {h.trigger:4d}",
            f"  grip     {bar(h.grip, 0, 4095)} {h.grip:4d}",
            f"  accel    {h.accel[0]:+6d} {h.accel[1]:+6d} {h.accel[2]:+6d}",
            f"  gyro     {h.gyro[0]:+6d} {h.gyro[1]:+6d} {h.gyro[2]:+6d}",
            f"  other    " + " ".join(f"{k}:{v.hex()}" for k, v in sorted(h.raw.items()) if k > 5),
            "",
        ]
    lines.append("HID report " + host.report().hex(" "))
    if stats:
        lines.append("link: " + " ".join(f"{k}={v}" for k, v in stats.items()))
    print("\n".join(lines), flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--raw", action="store_true")
    ap.add_argument("--log", action="store_true")
    ap.add_argument("--restart", action="store_true")
    ap.add_argument("--changes", action="store_true")
    ap.add_argument("--watch", action="store_true")
    args = ap.parse_args()

    p = json.load(open(PAIRING_FILE))
    b = Bridge()
    print("dongle:", b.ping())
    stored, running = b.pair_get()
    if stored != p["network_id"]:
        ok = b.pair_store(p["network_id"], bytes.fromhex(p["link_key"]))
        print(f"saved pairing {p['network_id']:#010x} to the dongle: {'ok' if ok else 'FAILED'}")
    elif args.restart or not running:
        b.link_resume()
    print(f"dongle hosting network {p['network_id']:#010x}; turn the controllers on")

    host = Host(args.raw, args.log, args.changes)
    host.quiet = args.watch
    last_stats = last_draw = time.time()
    try:
        while True:
            for f in b.frames(0.004):
                host.handle(f)
            if args.watch and time.time() - last_draw > 0.05:
                last_draw = time.time()
                watch_panel(host, host.stats)
            if time.time() - last_stats > 2:
                last_stats = time.time()
                b.send(b"\x21")
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
