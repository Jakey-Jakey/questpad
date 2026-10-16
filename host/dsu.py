"""DSU (Cemuhook) motion server: each Touch controller's gyro and accelerometer for emulators.

Cemu, Dolphin, Ryujinx and other DSU clients connect over UDP (127.0.0.1:26760, their default) and get
each controller's motion plus the combined buttons and sticks. DSU slot 1 is the gyro controller (the one
DS4 mode uses: right, unless "Gyro from the LEFT controller" is set or only the left one is on), slot 2
the other.

The control panel runs it built in ("Motion for emulators"). Without the panel:
    python3 host/dsu.py [--port 26760]
It reads the dongle's serial port, so it works in HID and DS4 modes (XInput mode has none), and on
Windows not while the panel is open (one program at a time can open the port).

Protocol: https://github.com/v1993/cemuhook-protocol. Axis signs match DS4Windows' DSU server with a
DualShock 4, which every client is tuned for.
"""
import argparse
import math
import os
import random
import socket
import struct
import sys
import threading
import time
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
try:
    import serial
except ImportError:
    sys.path.insert(0, os.path.join(HERE, "vendor"))
    import serial

from bridge import Bridge  # noqa: E402
from mapping import ANALOG_MAX, LOG_ITEM_LEN, Hand  # noqa: E402

HOST, PORT = "127.0.0.1", 26760
PROTOCOL = 1001
MSG_VERSION, MSG_INFO, MSG_DATA = 0x100000, 0x100001, 0x100002
SLOTS = 4  # DSU slots a client may ask about; QuestPad fills the first two
CLIENT_TIMEOUT = 5.0  # s: a client that stops asking stops getting data (as in DS4Windows)
SIDES = {0x13001101: "left", 0x13011101: "right"}  # link join device types (link.py)
MACS = {"left": bytes.fromhex("515000000001"), "right": bytes.fromhex("515000000002")}  # "QP" + hand

# Touch IMU scale (PROTOCOL.md), and the firmware's defaults for the settings used here (settings.rs).
GYRO_PER_DPS, ACCEL_PER_G = 8.192, 1024.0
STICK_FULL = 32000
DEFAULTS = dict(deadzone_pct=3, grip_pct=50, single_mode=1, gyro_left=0, imu_map_x=0x00, imu_map_y=0x82,
                imu_map_z=0x01)

# Data packet buttons, as (byte, mask): byte 0 = D-pad / Options / R3 / L3 / Share, 1 = face (DS4 names;
# clients call them Y B A X) and shoulders, 2 = Home. Touch button bits: 1 X / A, 2 Y / B, 4 stick, 8 menu.
OPTIONS, R3, L3 = (0, 0x08), (0, 0x04), (0, 0x02)
SQUARE, CROSS, CIRCLE, TRIANGLE = (1, 0x80), (1, 0x40), (1, 0x20), (1, 0x10)
R1, L1, R2, L2 = (1, 0x08), (1, 0x04), (1, 0x02), (1, 0x01)
HOME = (2, 0x01)
LEFT_BUTTONS = {1: SQUARE, 2: TRIANGLE, 4: L3, 8: OPTIONS}
RIGHT_BUTTONS = {1: CROSS, 2: CIRCLE, 4: R3, 8: HOME}
MINI_BUTTONS = {1: CROSS, 2: CIRCLE, 4: L3, 8: OPTIONS}  # one controller alone, single mode on


def ds4_frame(v, side, imu_map):
    """Touch IMU axes -> the DS4 frame (x right, y up, z toward the player), as the firmware does it:
    left samples into the right controller's axes (gamepad.rs), then `imu_map` (ds4.rs)."""
    if side == "left":
        v = (-v[1], v[0], v[2])
    return tuple(-v[min(m & 3, 2)] if m & 0x80 else v[min(m & 3, 2)] for m in imu_map)


def motion(side, accel, gyro, imu_map):
    """-> (accel x, y, z in g; gyro pitch, yaw, roll in deg/s), signed the way DS4Windows sends a
    DualShock 4's: accel all negated, yaw and roll negated."""
    ax, ay, az = ds4_frame(accel, side, imu_map)
    gx, gy, gz = ds4_frame(gyro, side, imu_map)
    return ((-ax / ACCEL_PER_G, -ay / ACCEL_PER_G, -az / ACCEL_PER_G),
            (gx / GYRO_PER_DPS, -gy / GYRO_PER_DPS, -gz / GYRO_PER_DPS))


def battery_code(pct):
    if pct is None:
        return 0
    return 1 if pct <= 5 else 2 if pct <= 20 else 3 if pct <= 60 else 4 if pct <= 90 else 5


def stick(x, y, deadzone):
    """Radial deadzone rescaled from its edge, as the firmware's -> DSU bytes (128 centre, +y up)."""
    r = math.hypot(x, y)
    if r <= deadzone:
        return 128, 128
    k = (min(r, STICK_FULL) - deadzone) / ((STICK_FULL - deadzone) * r)
    return tuple((int(v * k * 32767) >> 8) + 128 for v in (x, y))


def trigger(raw):
    return raw * 255 // ANALOG_MAX


def pad_bytes(hands, s):
    """Data packet bytes 16..47: the controllers [(side, Hand)] as one DS4, laid out like the firmware's
    combined pad (gamepad.rs `build`, ds4.rs `report`). No touchpad."""
    dz = STICK_FULL * s["deadzone_pct"] // 100
    grip_on = ANALOG_MAX * s["grip_pct"] // 100
    b = [0, 0, 0]
    lt = rt = 0
    lx = ly = rx = ry = 128
    single = len(hands) == 1 and s["single_mode"]
    for side, h in hands:
        x, y = stick(*h.stick, dz)
        if single:
            # Mini pad: stick = left stick, face = A / B, trigger = RT, grip = LT, menu = Start.
            names, (lx, ly), rt, lt = MINI_BUTTONS, (x, y), trigger(h.trigger), trigger(h.grip)
        elif side == "left":
            names, (lx, ly), lt = LEFT_BUTTONS, (x, y), trigger(h.trigger)
        else:
            names, (rx, ry), rt = RIGHT_BUTTONS, (x, y), trigger(h.trigger)
        for bit, (i, mask) in names.items():
            if h.buttons & bit:
                b[i] |= mask
        if not single and h.grip > grip_on:
            i, mask = L1 if side == "left" else R1
            b[i] |= mask
    b[1] |= (L2[1] if lt > 20 else 0) | (R2[1] if rt > 20 else 0)
    analog = [255 if b[i] & m else 0 for i, m in (SQUARE, CROSS, CIRCLE, TRIANGLE, R1, L1)]
    return struct.pack("<8B12B12x", b[0], b[1], b[2], 0, lx, ly, rx, ry, 0, 0, 0, 0, *analog, rt, lt)


class Controller:
    """One Touch controller on the dongle's link."""

    def __init__(self, side):
        self.side, self.hand, self.battery = side, Hand(), None


class Clock:
    """Motion timestamps for one DSU slot: the controller's own clock (us), kept rising when the slot
    switches controller or a controller restarts (clients drop samples that go back in time)."""

    def __init__(self):
        self.src, self.raw, self.offset, self.out = None, 0, 0, 0

    def __call__(self, src, raw):
        if src is not self.src or raw < self.raw:
            self.offset = max(0, self.out + 2000 - raw)
            self.src = src
        self.raw, self.out = raw, raw + self.offset
        return self.out


class Client:
    """One DSU client (an emulator) and what it asked for, with when it last asked."""

    def __init__(self):
        self.all, self.slots, self.macs, self.sent = 0.0, {}, {}, 0

    def wants(self, slot, mac, now):
        return now - max(self.all, self.slots.get(slot, 0), self.macs.get(mac, 0)) < CLIENT_TIMEOUT

    def alive(self, now):
        return now - max((self.all, *self.slots.values(), *self.macs.values())) < CLIENT_TIMEOUT


class Server:
    """DSU server fed with the dongle's serial frames (`feed`, e.g. as `Bridge.tap`). Raises OSError if
    the port is taken (by another DSU server: DS4Windows, BetterJoy, a second QuestPad)."""

    def __init__(self, host=HOST, port=PORT):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            self.sock.bind((host, port))
        except OSError:
            self.sock.close()
            raise
        self.host, self.port = self.sock.getsockname()
        self.sock.settimeout(0.5)
        self.id = random.getrandbits(32)
        self._settings = dict(DEFAULTS)
        self.ctls = {}  # dongle link slot -> Controller
        self.clients = {}  # address -> Client
        self.clocks = [Clock(), Clock()]
        self.shown = [None, None]  # controller each DSU slot last streamed
        self.lock = threading.RLock()
        self.running = True
        threading.Thread(target=self._serve, daemon=True).start()

    @property
    def settings(self):
        return self._settings

    @settings.setter
    def settings(self, s):
        """The dongle's settings (Bridge.settings_get), for the gyro choice, imu_map and pad layout."""
        with self.lock:
            self._settings = {**DEFAULTS, **{k: s[k] for k in DEFAULTS if k in s}}
            self._check()

    # ---- dongle side ----

    def feed(self, f):
        """One frame from the dongle: link joins / drops / data, and status replies (sides, battery)."""
        kind = f[0]
        with self.lock:
            if kind == 0xA1 and f[3]:
                c, p = self.ctls.get(f[1]), f[4:]
                if c is None or len(p) <= 3 + LOG_ITEM_LEN or not p[2] & 0x40:
                    return
                ts = c.hand.imu_ts
                c.hand.update(p[3 + LOG_ITEM_LEN :])
                if c.hand.imu_ts != ts:
                    self._stream(c)
            elif kind == 0xA0 and len(f) >= 14:
                side = SIDES.get(struct.unpack_from("<I", f, 10)[0])
                if side:
                    self.ctls[f[1]] = Controller(side)
                    self._check()
            elif kind == 0xA2:
                self.ctls.pop(f[1], None)
                self._check()
            elif kind == 0xA9 and len(f) >= 5:
                live = {}
                for slot, side, battery in Bridge.parse_status(f)["controllers"]:
                    c = self.ctls.get(slot)
                    live[slot] = c = c if c and c.side == side else Controller(side)
                    c.battery = battery
                self.ctls = live
                self._check()

    def clear(self):
        """The dongle went away."""
        with self.lock:
            self.ctls = {}
            self._check()

    def slots(self):
        """Controllers in DSU slots 1 and 2: the gyro controller (picked as DS4 mode does), then the other."""
        by_side = {c.side: c for c in self.ctls.values()}
        want = "left" if self._settings["gyro_left"] else "right"
        if want not in by_side:
            return [by_side.get("right" if want == "left" else "left"), None]
        return [by_side[want], by_side.get("right" if want == "left" else "left")]

    def _stream(self, c):
        slots = self.slots()
        if c not in slots:
            return
        i = slots.index(c)
        self.shown[i] = c
        if not self.clients:
            return
        s = self._settings
        accel, gyro = motion(c.side, c.hand.accel, c.hand.gyro, (s["imu_map_x"], s["imu_map_y"], s["imu_map_z"]))
        pad = pad_bytes([(x.side, x.hand) for x in self.ctls.values()], s)
        payload = (self._info(i, c) + struct.pack("<BI", 1, 0) + pad
                   + struct.pack("<Q6f", self.clocks[i](c, c.hand.imu_ts), *accel, *gyro))
        self._send_slot(i, MACS[c.side], payload)

    def _check(self):
        """Tell clients when a DSU slot loses its controller: they then keep asking for it, so motion
        resumes by itself when one is back."""
        for i, c in enumerate(self.slots()):
            gone = self.shown[i]
            if gone is not None and c is not gone:
                self.shown[i] = None
                if c is None:
                    self._send_slot(i, MACS[gone.side], self._info(i, None) + bytes(69))

    def _info(self, i, c):
        if c is None:
            return struct.pack("<4B6sB", i, 0, 0, 0, bytes(6), 0)
        return struct.pack("<4B6sB", i, 2, 2, 1, MACS[c.side], battery_code(c.battery))  # connected, gyro, USB

    # ---- network side ----

    def _send(self, addr, msg, payload):
        body = struct.pack("<I", msg) + payload
        head = struct.pack("<4sHH", b"DSUS", PROTOCOL, len(body))
        tail = struct.pack("<I", self.id) + body
        try:
            self.sock.sendto(head + struct.pack("<I", zlib.crc32(head + bytes(4) + tail)) + tail, addr)
        except OSError:
            pass

    def _send_slot(self, i, mac, payload):
        now = time.monotonic()
        for addr, cl in list(self.clients.items()):
            if not cl.alive(now):
                del self.clients[addr]
            elif cl.wants(i, mac, now):
                body = bytearray(payload)
                struct.pack_into("<I", body, 12, cl.sent & 0xFFFFFFFF)  # per-client packet counter
                cl.sent += 1
                self._send(addr, MSG_DATA, bytes(body))

    def _serve(self):
        while self.running:
            try:
                data, addr = self.sock.recvfrom(2048)
            except TimeoutError:
                continue
            except OSError:
                # Windows reports a send to a client that has quit as an error on the next receive.
                if not self.running:
                    return
                continue
            with self.lock:
                self._request(data, addr)

    def _request(self, data, addr):
        if len(data) < 20 or data[:4] != b"DSUC":
            return
        length = struct.unpack_from("<H", data, 6)[0]
        if length < 4 or len(data) < 16 + length:
            return
        data = data[: 16 + length]
        if zlib.crc32(data[:8] + bytes(4) + data[12:]) != struct.unpack_from("<I", data, 8)[0]:
            return
        msg, p = struct.unpack_from("<I", data, 16)[0], data[20:]
        if msg == MSG_VERSION:
            self._send(addr, MSG_VERSION, struct.pack("<H", PROTOCOL))
        elif msg == MSG_INFO and len(p) >= 4:
            n = max(0, min(struct.unpack_from("<i", p)[0], SLOTS))
            slots = self.slots()
            for i in p[4 : 4 + n]:
                if i < SLOTS:
                    self._send(addr, MSG_INFO, self._info(i, slots[i] if i < 2 else None) + b"\0")
        elif msg == MSG_DATA:
            p = p.ljust(8, b"\0")
            flags, i, mac = p[0], p[1], bytes(p[2:8])
            cl = self.clients.setdefault(addr, Client())
            now = time.monotonic()
            if flags == 0:
                cl.all = now
            if flags & 1:
                cl.slots[i] = now
            if flags & 2:
                cl.macs[mac] = now

    # ---- state for the panel ----

    def status(self):
        """-> (clients that asked in the last few seconds, [side or None for DSU slots 1 and 2])."""
        with self.lock:
            now = time.monotonic()
            return sum(cl.alive(now) for cl in self.clients.values()), [c and c.side for c in self.slots()]

    def describe(self):
        n, sides = self.status()
        who = f"{n} emulator{'s' if n > 1 else ''} connected" if n else "no emulator connected yet"
        on = [f"slot {i + 1}: {side}" for i, side in enumerate(sides) if side]
        return f"{who} - " + (", ".join(on) if on else "no controllers")

    def close(self):
        """Stop. Clients see every slot disconnect, so they ask again once a server is back."""
        with self.lock:
            self.clear()
            self.running = False
            self.sock.close()


def main():
    ap = argparse.ArgumentParser(description="DSU (Cemuhook) motion server for the Touch controllers.")
    ap.add_argument("--host", default=HOST, help=f"address to listen on (default {HOST}; 0.0.0.0 = network)")
    ap.add_argument("--port", type=int, default=PORT, help=f"UDP port (default {PORT})")
    args = ap.parse_args()
    try:
        b = Bridge()
    except serial.SerialException as e:
        sys.exit(f"Can't open the dongle ({e}). Close the control panel (or turn motion on there instead).")
    try:
        server = Server(args.host, args.port)
    except OSError as e:
        sys.exit(f"Can't listen on {args.host}:{args.port} ({e}). Is another DSU server running (DS4Windows, "
                 f"BetterJoy, the QuestPad panel)?")
    print("dongle:", b.ping()[1])
    server.settings = b.settings_get()
    b.tap = server.feed
    print(f"DSU server on {server.host}:{server.port} (Ctrl-C to stop)")
    shown = None
    try:
        while True:
            b.status()  # its reply tells the server which controllers are on, and their battery
            line = server.describe()
            if line != shown:
                print(line)
                shown = line
            for _ in b.frames(1.0):
                pass
    except KeyboardInterrupt:
        pass
    finally:
        server.close()


if __name__ == "__main__":
    main()
