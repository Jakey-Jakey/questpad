"""A DSU client, like Cemu or Dolphin, for checking the motion server (host/dsu.py or the panel's).

  python3 host/dsu_client.py             show what each slot sends, live (Ctrl-C to quit)
  python3 host/dsu_client.py --guided    walk through a few poses and turns, checking each motion direction
  python3 host/dsu_client.py --selftest  run a server on a spare port with made-up controllers and check
                                         every packet (no dongle needed)
  --port N                               server port (default 26760)

Directions, as DS4Windows sends a DualShock 4's: accel in g shows gravity at rest (y -1 held as if playing, z -1
pointing at the floor, x +1 right side down); gyro in deg/s is positive tipping the front up (pitch), turning right
(yaw) and twisting clockwise (roll).
"""
import argparse
import collections
import math
import os
import socket
import struct
import sys
import time
import zlib

VERSION, INFO, DATA = 0x100000, 0x100001, 0x100002
SIDES = {1: "left", 2: "right"}  # last MAC byte (dsu.MACS)


def check(ok, what):
    if not ok:
        raise AssertionError(what)


class Client:
    def __init__(self, port=26760, host="127.0.0.1", timeout=1.0):
        self.addr = (host, port)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(timeout)

    def send(self, msg, payload=b"", corrupt=False):
        body = struct.pack("<I", msg) + payload
        pkt = bytearray(b"DSUC" + struct.pack("<HHII", 1001, len(body), 0, 0xC0FFEE) + body)
        struct.pack_into("<I", pkt, 8, zlib.crc32(pkt) ^ corrupt)
        self.sock.sendto(bytes(pkt), self.addr)

    def ask_info(self, slots=(0, 1, 2, 3)):
        self.send(INFO, struct.pack("<i", len(slots)) + bytes(slots))

    def subscribe(self, slot=None, mac=None):
        flags = (slot is not None) | (mac is not None) << 1
        self.send(DATA, struct.pack("<BB6s", flags, slot or 0, mac or bytes(6)))

    def recv(self, timeout=None):
        """-> (message type, payload) of the next server packet, checking its header and CRC; None if
        nothing arrives within `timeout` (default: the socket's)."""
        if timeout is not None:
            self.sock.settimeout(timeout)
        try:
            data, _ = self.sock.recvfrom(2048)
        except (TimeoutError, ConnectionResetError):
            return None
        check(len(data) >= 20 and data[:4] == b"DSUS", f"bad magic {data[:4]!r}")
        version, length, crc = struct.unpack_from("<HHI", data, 4)
        check(version == 1001, f"protocol version {version}")
        check(len(data) == 16 + length, f"length field {length} for a {len(data)}-byte packet")
        check(zlib.crc32(data[:8] + bytes(4) + data[12:]) == crc, "CRC mismatch")
        return struct.unpack_from("<I", data, 16)[0], data[20:]


def parse_info(p):
    slot, state, model, conn, mac, battery = struct.unpack_from("<4B6sB", p)
    return dict(slot=slot, state=state, model=model, conn=conn, mac=mac, battery=battery,
                side=SIDES.get(mac[5]) if state else None)


def parse_data(p):
    check(len(p) == 80, f"data payload is {len(p)} bytes, not 80")
    d = parse_info(p)
    d["connected"], d["counter"] = struct.unpack_from("<BI", p, 11)
    d["b1"], d["b2"], d["home"], d["touch"], d["lx"], d["ly"], d["rx"], d["ry"] = p[16:24]
    d["analog"] = list(p[24:36])
    d["ts"], *motion = struct.unpack_from("<Q6f", p, 48)
    d["accel"], d["gyro"] = tuple(motion[:3]), tuple(motion[3:])
    return d


# ---- live view ----

def live(port):
    c = Client(port, timeout=0.05)
    c.send(VERSION)
    reply = c.recv(1.0)
    if reply is None:
        sys.exit(f"No DSU server answers on 127.0.0.1:{port}. Turn on 'Motion for emulators' in the QuestPad "
                 "panel, or run host/dsu.py.")
    print("server protocol version", struct.unpack_from("<H", reply[1])[0])
    last, rate, count, prev_ts = {}, {}, {}, {}
    t_ask = t_draw = t_rate = 0.0
    while True:
        now = time.monotonic()
        if now - t_ask > 1.0:
            c.ask_info()
            c.subscribe()
            t_ask = now
        r = c.recv(0.02)
        if r and r[0] == DATA:
            d = parse_data(r[1])
            d["dt"] = d["ts"] - prev_ts.get(d["slot"], d["ts"])
            prev_ts[d["slot"]] = d["ts"]
            last[d["slot"]] = d
            count[d["slot"]] = count.get(d["slot"], 0) + 1
        if now - t_rate >= 1.0:
            rate, count, t_rate = count, {}, now
        if now - t_draw > 0.1:
            t_draw = now
            lines = ["\x1b[H\x1b[2J" + f"DSU server 127.0.0.1:{port} (Ctrl-C to quit)", ""]
            for slot in (0, 1):
                d = last.get(slot)
                if not d or not d["connected"]:
                    lines += [f"slot {slot + 1}: no controller", ""]
                    continue
                lines += [
                    f"slot {slot + 1}: {d['side']} controller  battery code {d['battery']}  "
                    f"{rate.get(slot, 0)} packets/s  timestamp step {d['dt']} us",
                    "  accel  x {:+6.2f}  y {:+6.2f}  z {:+6.2f}  g".format(*d["accel"]),
                    "  gyro   pitch {:+7.1f}  yaw {:+7.1f}  roll {:+7.1f}  deg/s".format(*d["gyro"]),
                    f"  buttons {d['b1']:08b} {d['b2']:08b} home {d['home']}  sticks {d['lx']:3d},{d['ly']:3d}"
                    f"  {d['rx']:3d},{d['ry']:3d}  L2 {d['analog'][11]:3d}  R2 {d['analog'][10]:3d}",
                    "",
                ]
            print("\n".join(lines), flush=True)


# ---- guided direction check with real controllers ----
# First the controllers rest on a desk, where the server measures their gyro drift. Then each step ends in a pose
# held still: the accelerometer shows which way is down, and the gyro, integrated over the whole move into the pose,
# shows how the controller turned and must turn gravity the way the accelerometer saw it.

GUIDE = [  # (instruction, expected accel axis and sign at the pose, expected turn axis and sign of the move)
    ("Put both controllers down on a desk and leave them for 3 seconds (QuestPad measures their drift).",
     None, None),
    ("Pick them up and hold both as if playing, pointing ahead. Keep them still.", (1, -1), None),
    ("Tip both forward to point straight down at the floor, and keep them still.", (2, -1), (0, -1)),
    ("Bring both back to the playing position, and keep them still.", (1, -1), (0, 1)),
    ("Twist both clockwise, like turning a key, until their right side faces the floor. Keep them still.",
     (0, 1), (2, 1)),
    ("Twist them back to the playing position, and keep them still.", (1, -1), (2, -1)),
    ("Keeping them level, turn both 90 degrees to your right (a swivel chair helps). Keep them still.",
     (1, -1), (1, 1)),
]
ACCEL_AXES, TURNS = ("x", "y", "z"), ("pitch", "yaw", "roll")
STEADY_DPS, MOVING_DPS, STEADY_SAMPLES, DESK_SAMPLES = 6, 30, 350, 1500  # deg/s; samples (500 per s)
MIN_CHANGE, STEP_TIMEOUT, MAX_DRIFT, MAX_MISMATCH = 45, 60, 0.5, 30  # deg; s; deg/s; deg


def qmul(a, b):
    w0, x0, y0, z0 = a
    w1, x1, y1, z1 = b
    return (w0 * w1 - x0 * x1 - y0 * y1 - z0 * z1, w0 * x1 + x0 * w1 + y0 * z1 - z0 * y1,
            w0 * y1 - x0 * z1 + y0 * w1 + z0 * x1, w0 * z1 + x0 * y1 - y0 * x1 + z0 * w1)


def to_body(q, v):
    """World vector v in the frame of a body turned by q."""
    w, x, y, z = q
    r = [[1 - 2 * (y * y + z * z), 2 * (x * y - w * z), 2 * (x * z + w * y)],
         [2 * (x * y + w * z), 1 - 2 * (x * x + z * z), 2 * (y * z - w * x)],
         [2 * (x * z - w * y), 2 * (y * z + w * x), 1 - 2 * (x * x + y * y)]]
    return [sum(r[i][j] * v[i] for i in range(3)) for j in range(3)]


def angle(a, b):
    na, nb = math.sqrt(sum(x * x for x in a)), math.sqrt(sum(x * x for x in b))
    return math.degrees(math.acos(max(-1.0, min(1.0, sum(x * y for x, y in zip(a, b)) / (na * nb)))))


class Track:
    """One slot's motion since its last pose: steadiness, and the gyro integrated in the DS4 frame (DSU signs
    undone: gravity is -accel, body rate is pitch, -yaw, -roll)."""

    def __init__(self):
        self.pose = None  # (gravity, mean gyro) of the last pose
        self.reset()

    def reset(self):
        self.q, self.last_ts = (1.0, 0.0, 0.0, 0.0), None
        self.levels, self.window = collections.deque(maxlen=50), collections.deque(maxlen=STEADY_SAMPLES)
        self.level_sum, self.sums, self.run = 0.0, [0.0] * 6, 0  # running sums over levels / window; steady run
        self.moved = False

    def add(self, d):
        if self.last_ts is not None and 0 < d["ts"] - self.last_ts < 50000:
            dt = (d["ts"] - self.last_ts) / 1e6
            p, y, r = d["gyro"]  # the server already removes the drift it measured on the desk
            w = [math.radians(p), -math.radians(y), -math.radians(r)]
            th = math.sqrt(sum(v * v for v in w)) * dt
            if th > 0:
                s = math.sin(th / 2) / (th / dt)
                self.q = qmul(self.q, (math.cos(th / 2), w[0] * s, w[1] * s, w[2] * s))
        self.last_ts = d["ts"]
        if len(self.levels) == self.levels.maxlen:
            self.level_sum -= self.levels[0]
        self.levels.append(max(map(abs, d["gyro"])))
        self.level_sum += self.levels[-1]
        if len(self.window) == self.window.maxlen:
            old = self.window[0]
            self.sums = [a - b for a, b in zip(self.sums, (*old["accel"], *old["gyro"]))]
        self.window.append(d)
        self.sums = [a + b for a, b in zip(self.sums, (*d["accel"], *d["gyro"]))]
        level = self.level_sum / len(self.levels)
        self.run = self.run + 1 if level < STEADY_DPS else 0
        self.moved |= level > MOVING_DPS

    def steady(self, hold=STEADY_SAMPLES):
        """-> (gravity, mean gyro) over the last STEADY_SAMPLES if still for `hold` samples, else None."""
        if self.run < hold:
            return None
        n = len(self.window)
        return [-v / n for v in self.sums[:3]], [v / n for v in self.sums[3:]]

    def turned(self):
        """-> the move so far as DSU (pitch, yaw, roll) degrees."""
        th = math.degrees(2 * math.acos(max(-1.0, min(1.0, self.q[0]))))
        sn = math.sqrt(sum(v * v for v in self.q[1:])) or 1
        return [th * self.q[1] / sn, -th * self.q[2] / sn, -th * self.q[3] / sn]


def guided(port):
    c = Client(port, timeout=0.05)
    c.send(VERSION)
    if c.recv(1.0) is None:
        sys.exit(f"No DSU server answers on 127.0.0.1:{port}. Run host/dsu.py (or turn on motion in the panel).")
    t_ask = 0.0

    def samples():
        """Each data packet as it arrives (None every 10 ms without one), renewing the subscription."""
        nonlocal t_ask
        while True:
            if time.monotonic() - t_ask > 1.0:
                c.subscribe()
                t_ask = time.monotonic()
            r = c.recv(0.01)
            yield parse_data(r[1]) if r and r[0] == DATA else None

    print("Waiting for controllers (press a button on each to wake it)...")
    sides, seen = {}, samples()
    end = time.monotonic() + 60
    for d in seen:
        if d and d["connected"] and d["slot"] not in sides:
            sides[d["slot"]] = d["side"]
            end = min(end, time.monotonic() + 3)  # give the other one a moment to show up too
        if time.monotonic() > end:
            break
    if not sides:
        sys.exit("No controller data. Wake the controllers (press a button) and try again.")
    print("Checking:", ", ".join(f"slot {s + 1} = {side}" for s, side in sorted(sides.items())))
    check_steps(seen, sides)


def check_steps(seen, sides):
    """Walk through GUIDE on a stream of data packets (None = nothing yet) from the slots in `sides`."""
    tracks = {s: Track() for s in sides}
    failures = []
    for text, expect, turn in GUIDE:
        print("\n" + text)
        done, start, clock = {}, None, None  # step time runs on the controllers' clock (us)
        for d in seen:
            if len(done) == len(sides) or (clock is not None and clock - start > STEP_TIMEOUT * 1e6):
                break
            if not d or d["slot"] not in sides or not d["connected"]:
                continue
            clock = max(clock or 0, d["ts"])
            start = clock if start is None else start
            t = tracks[d["slot"]]
            t.add(d)  # keeps integrating after this step is done for it: that motion belongs to the next move
            if d["slot"] in done:
                continue
            pose = t.steady(DESK_SAMPLES if expect is None else STEADY_SAMPLES)
            if not pose:
                continue
            tilted = t.pose is not None and angle(t.pose[0], pose[0]) > MIN_CHANGE
            if turn is None:  # no move to check: any pose held still once the controller has moved
                accept = t.pose is None or t.moved
            elif turn[0] == 1:  # a turn about the vertical leaves gravity as it was
                accept = not tilted and max(map(abs, t.turned())) > MIN_CHANGE
            else:
                accept = tilted
            if accept:
                done[d["slot"]] = (pose, t.turned(), t.pose and angle(to_body(t.q, t.pose[0]), pose[0]))
                t.pose = pose
                t.reset()
            elif not tilted:
                t.reset()  # still in the previous pose: the move starts from here (keeps waiting time's wobble out)
        for s, side in sorted(sides.items()):
            if s not in done:
                print(f"  {side:5s} no steady pose within {STEP_TIMEOUT} s")
                failures.append(f"{side}: {text} (timed out)")
                continue
            (gravity, gyro), rv, mismatch = done[s]
            if expect is None:
                drift = max(map(abs, gyro))
                print(f"  {side:5s} gyro at rest {' '.join(f'{v:+.2f}' for v in gyro)} deg/s "
                      + ("ok" if drift < MAX_DRIFT else "NOT CORRECTED (is the server older than this client?)"))
                if drift >= MAX_DRIFT:
                    failures.append(f"{side}: gyro drift {drift:.2f} deg/s at rest")
                continue
            axis, sign = expect
            down = [-v for v in gravity]  # DSU accel at rest points down
            big = max(range(3), key=lambda i: abs(down[i]))
            ok = big == axis and (down[big] > 0) == (sign > 0)
            line = f"accel {ACCEL_AXES[big]} {down[big]:+.2f} g " + (
                "ok" if ok else f"WRONG (expected {ACCEL_AXES[axis]} {sign:+d})")
            if not ok:
                failures.append(f"{side}: {text} -> accel {ACCEL_AXES[big]} {down[big]:+.2f}")
            if turn:
                big = max(range(3), key=lambda i: abs(rv[i]))
                ok = big == turn[0] and (rv[big] > 0) == (turn[1] > 0)
                line += f";  turned {TURNS[big]} {rv[big]:+.0f} deg " + (
                    "ok" if ok else f"WRONG (expected {TURNS[turn[0]]} {turn[1]:+d})")
                line += f";  gyro vs gravity {mismatch:.0f} deg off"
                if not ok:
                    failures.append(f"{side}: {text} -> {TURNS[big]} {rv[big]:+.0f}")
                if mismatch > MAX_MISMATCH:
                    failures.append(f"{side}: {text} -> gyro and gravity disagree by {mismatch:.0f} deg")
            print(f"  {side:5s} {line}")
    print("\nAll directions match." if not failures else "\nProblems:\n  " + "\n  ".join(failures))


# ---- self-test against an in-process server fed with made-up dongle frames ----

def link_frame(slot, ts=None, accel=(0, 0, 0), gyro=(0, 0, 0), buttons=None, stick=None, trigger=None, grip=None):
    from mapping import encode_notification
    ntf = b""
    if ts is not None:
        ntf += encode_notification(1, ts.to_bytes(6, "little") + struct.pack("<6h", *accel, *gyro))
    if buttons is not None:
        ntf += encode_notification(4, bytes([buttons | 0x10]))
    if stick is not None:
        ntf += encode_notification(2, struct.pack("<hh", *stick))
    if trigger is not None:
        t, g = 4095 - trigger, 4095 - grip  # sent inverted, two 12-bit values in 3 bytes
        ntf += encode_notification(3, bytes([t & 0xFF, t >> 8 | (g & 0x0F) << 4, g >> 4]))
    return bytes([0xA1, slot, 50, 1, slot, 0x01, 0x40]) + bytes(4) + ntf


def status_frame(*ctls):
    """ctls: (link slot, 'left'/'right', battery %)."""
    f = bytes([0xA9, 1, 1, 0, len(ctls)])
    for slot, side, battery in ctls:
        f += bytes([slot, side == "right", battery])
    return f + bytes(len(ctls))


def near(a, b):
    return all(abs(x - y) < 1e-3 for x, y in zip(a, b))


def selftest():
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import dsu

    server = dsu.Server(port=0)
    c = Client(server.port, timeout=0.3)
    data = lambda: parse_data(expect(DATA))  # noqa: E731

    def expect(msg):
        r = c.recv(1.0)
        check(r is not None, f"no reply (wanted {msg:#x})")
        check(r[0] == msg, f"message {r[0]:#x}, wanted {msg:#x}")
        return r[1]

    def silent(what):
        check(c.recv(0.15) is None, f"unexpected packet: {what}")

    c.send(VERSION)
    check(struct.unpack("<H", expect(VERSION)) == (1001,), "protocol version")
    c.send(VERSION, corrupt=True)
    silent("reply to a request with a bad CRC")

    c.ask_info()
    infos = [parse_info(expect(INFO)) for _ in range(4)]
    check([i["slot"] for i in infos] == [0, 1, 2, 3] and not any(i["state"] for i in infos), f"empty slots {infos}")

    # Both controllers on: link slot 1 left (80 %), slot 2 right (100 %). Right = DSU slot 1 (gyro default).
    server.feed(status_frame((1, "left", 80), (2, "right", 100)))
    c.ask_info()
    infos = [parse_info(expect(INFO)) for _ in range(4)]
    check((infos[0]["state"], infos[0]["model"], infos[0]["side"], infos[0]["battery"]) == (2, 2, "right", 5),
          f"slot 1 info {infos[0]}")
    check((infos[1]["side"], infos[1]["battery"]) == ("left", 4), f"slot 2 info {infos[1]}")
    check(not infos[2]["state"] and not infos[3]["state"], "slots 3-4 empty")

    server.feed(link_frame(2, ts=1_000_000, accel=(100, -200, -1024), gyro=(82, -164, 410)))
    silent("data before subscribing")

    # Subscribed to slot 1 only. Right controller (default imu_map): DSU accel = (-X, Z, -Y) / 1024,
    # gyro = (X, Z, -Y) / 8.192 (DS4 frame x = X, y = -Z, z = Y, then DS4Windows' DSU signs).
    c.subscribe(slot=0)
    time.sleep(0.05)
    server.feed(link_frame(2, ts=1_002_000, accel=(100, -200, -1024), gyro=(82, -164, 410), buttons=0x01,
                           stick=(32000, 0), trigger=4095, grip=4095))
    d = data()
    check((d["slot"], d["side"], d["connected"], d["counter"], d["ts"]) == (0, "right", 1, 0, 1_002_000),
          f"slot 1 header {d}")
    check(near(d["accel"], (-100 / 1024, -1.0, 200 / 1024)), f"right accel {d['accel']}")
    check(near(d["gyro"], (82 / 8.192, 410 / 8.192, 164 / 8.192)), f"right gyro {d['gyro']}")
    check(d["b2"] == 0x40 | 0x08 | 0x02 and d["analog"][5] == 255, f"A = Cross, grip = R1, trigger = R2: {d}")
    check((d["rx"], d["ry"], d["lx"], d["ly"], d["analog"][10]) == (255, 128, 128, 128, 255), f"right stick, R2 {d}")
    server.feed(link_frame(1, ts=5_000_000, accel=(300, 100, -1024), gyro=(10, 20, 30)))
    silent("slot 2 data while subscribed to slot 1 only")

    # Subscribe to everything. Left controller: turned into the right's axes (X, Y, Z) -> (-Y, X, Z) first,
    # so DSU accel = (Y, Z, -X) / 1024, gyro = (-Y, Z, -X) / 8.192.
    c.subscribe()
    time.sleep(0.05)
    server.feed(link_frame(1, ts=5_002_000, accel=(300, 100, -1024), gyro=(10, 20, 30), buttons=0x03))
    d = data()
    check((d["slot"], d["side"], d["counter"]) == (1, "left", 1), f"slot 2 header {d}")
    check(near(d["accel"], (100 / 1024, -1.0, -300 / 1024)), f"left accel {d['accel']}")
    check(near(d["gyro"], (-20 / 8.192, 30 / 8.192, -10 / 8.192)), f"left gyro {d['gyro']}")
    check(d["b2"] & 0xF0 == 0x80 | 0x10 | 0x40, f"X = Square, Y = Triangle, A still held = Cross: {d['b2']:#x}")

    # A controller clock going backwards (it restarted) still gives rising timestamps.
    server.feed(link_frame(2, ts=10_000, accel=(0, 0, -1024)))
    d = data()
    check(d["slot"] == 0 and d["ts"] > 1_002_000, f"timestamp after a controller restart {d['ts']}")
    ts1 = d["ts"]

    # Right controller drops: slot 2 says disconnected (so clients keep asking), the left one moves to slot 1
    # with timestamps that keep rising there.
    server.feed(bytes([0xA2, 2]))
    d = data()
    check((d["slot"], d["state"], d["connected"]) == (1, 0, 0), f"disconnect notice {d}")
    server.feed(link_frame(1, ts=5_004_000, accel=(0, 0, -1024)))
    d = data()
    check((d["slot"], d["side"]) == (0, "left") and d["ts"] > ts1, f"left controller in slot 1 {d}")

    # Right back, gyro_left on: left stays DSU slot 1, right takes slot 2.
    server.feed(status_frame((1, "left", 80), (2, "right", 100)))
    server.settings = dict(gyro_left=1)
    server.feed(link_frame(2, ts=1_004_000, accel=(0, 0, -1024)))
    d = data()
    check((d["slot"], d["side"]) == (1, "right"), f"gyro_left slot order {d}")
    check(server.status() == (1, ["left", "right"]), f"status {server.status()}")

    # One controller alone with single mode on: a whole mini pad (stick = left stick, grip = L2).
    server.feed(bytes([0xA2, 2]))
    expect(DATA)  # slot 2 disconnect
    server.feed(link_frame(1, ts=5_006_000, buttons=0x01, stick=(0, 32000), trigger=0, grip=4095))
    d = data()
    check((d["b2"] & 0x40, d["lx"], d["ly"], d["analog"][11], d["analog"][10]) == (0x40, 128, 255, 255, 0),
          f"mini pad {d}")

    # Gyro drift: held (wobbling) it stays; lying still for over a second it is measured and removed.
    def feed_many(n, ts0, gyro, wobble=0):
        for i in range(n):
            g = [v + (wobble if i % 2 else -wobble) for v in gyro]
            server.feed(link_frame(1, ts=ts0 + i * 2000, accel=(0, 0, -1024), gyro=g))
        last = None
        while (r := c.recv(0.1)) is not None:
            last = parse_data(r[1]) if r[0] == DATA else last
        return last
    feed_many(600, 6_000_000, (12, -8, 4), wobble=20)
    check(server.rest["left"].offset == (0, 0, 0), f"drift learned from a wobbling hand {server.rest['left'].offset}")
    d = feed_many(1, 7_300_000, (12, -8, 4))
    check(near(d["gyro"], (8 / 8.192, 4 / 8.192, -12 / 8.192)), f"left gyro before the drift is known {d['gyro']}")
    d = feed_many(800, 8_000_000, (12, -8, 4))
    check(near(d["gyro"], (0, 0, 0)), f"gyro at rest after the drift was measured {d['gyro']}")
    d = feed_many(1, 10_000_000, (12 + 82, -8, 4))
    check(near(d["gyro"], (0, 0, -82 / 8.192)), f"left turning after drift removal {d['gyro']}")  # left X = roll axis

    # A client that stops asking stops getting data.
    dsu.CLIENT_TIMEOUT = 0.2
    time.sleep(0.3)
    server.feed(link_frame(1, ts=5_008_000))
    silent("data after the client timed out")
    check(server.status()[0] == 0, "timed-out client still counted")
    dsu.CLIENT_TIMEOUT = 5.0

    # Stopping the server: subscribers see their slots disconnect.
    c.subscribe()
    time.sleep(0.05)
    server.feed(link_frame(1, ts=5_010_000))
    expect(DATA)
    server.close()
    d = data()
    check((d["slot"], d["state"]) == (0, 0), f"disconnect on close {d}")

    # Cost: one controller sample in, one packet out.
    server = dsu.Server(port=0)
    c = Client(server.port)
    server.feed(status_frame((1, "left", 80), (2, "right", 100)))
    c.subscribe()
    time.sleep(0.05)
    n, t0 = 2000, time.perf_counter()
    for i in range(n):
        server.feed(link_frame(1 + i % 2, ts=6_000_000 + i * 1000, accel=(i, -i, 1024), gyro=(i, i, i)))
    us = (time.perf_counter() - t0) / n * 1e6
    server.close()
    print(f"dsu selftest ok ({us:.0f} us per controller sample)")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--port", type=int, default=26760)
    ap.add_argument("--guided", action="store_true")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    try:
        guided(args.port) if args.guided else live(args.port)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
