"""Controller notifications -> one combined Xbox-style gamepad report.

Uplink TL notification frame (decrypted): [slot][tl id][0x40 | seq] [log item 4 B] [entries...]
(the 4-byte log item tunnels the controller's debug log, `wireless_thread FUN_00010c20`).
Notification entries (controller `FUN_0001c408`): repeated
    [id:5 | len_lo:3 << 5] [len_hi:4 | seq:3 << 4 | last:1 << 7] data[len]
IDs, verified on air with the left controller (2026-10-10):
    1  IMU 18 B     ts u48 (us), accel xyz i16 (~1000/g), gyro xyz i16; one per 2 ms
    2  thumbstick   x, y i16 (+x right, +y up, full scale ~+-32500); sent as 0,0 when centred
    3  trigger/grip two 12-bit values packed in 3 B, inverted: 4095 released, 0 fully squeezed
    4  buttons u8   bit0 X/A, bit1 Y/B, bit2 stick click, bit3 menu (left; right = Oculus?), bit4 always 1
    5  touch u8     capacitive bits (0x01 X, 0x02 Y, 0x04 stick, 0x08 trigger; 0x10/0x20/0x40/0x80 near the same)
    0  battery u8   percent
    6 u8, 7 4 B     TBD (7 is noisy, capsense raw?)
Right-controller bit roles are assumed to mirror the left (A/B/stick/Oculus) until checked.
"""
import struct
from dataclasses import dataclass, field

NTF_BATTERY, NTF_IMU, NTF_STICK, NTF_ANALOG, NTF_BUTTONS, NTF_TOUCH = 0, 1, 2, 3, 4, 5
LOG_ITEM_LEN = 4
ANALOG_MAX = 4095

# Gamepad button bits (gamepad.rs).
A, B, X, Y, LB, RB, BACK, START, LS, RS, GUIDE = (1 << i for i in range(11))


def parse_notifications(buf):
    """Yield (id, seq, last, data) for each entry in a notification payload."""
    i = 0
    while i + 2 <= len(buf):
        b0, b1 = buf[i], buf[i + 1]
        nid = b0 & 0x1F
        length = (b0 >> 5) | ((b1 & 0x0F) << 3)
        seq, last = (b1 >> 4) & 7, b1 >> 7
        if nid == 0 and length == 0:
            break  # padding
        data = buf[i + 2 : i + 2 + length]
        if len(data) < length:
            break
        yield nid, seq, last, data
        i += 2 + length


def encode_notification(nid, data, seq=0, last=0):
    """Inverse of parse_notifications (matches the controller's encoder; for tests)."""
    n = len(data)
    return bytes([(nid & 0x1F) | ((n & 7) << 5), ((n & 0x7F) >> 3) | ((seq & 7) << 4) | ((last & 1) << 7)]) + data


def unpack12(d):
    """Two 12-bit values packed little-endian in 3 bytes."""
    return d[0] | ((d[1] & 0x0F) << 8), (d[1] >> 4) | (d[2] << 4)


@dataclass
class Hand:
    buttons: int = 0
    touch: int = 0
    stick: tuple = (0, 0)  # i16, +y up
    trigger: int = 0  # 0..4095 squeezed
    grip: int = 0
    battery: int = 0
    accel: tuple = (0, 0, 0)
    gyro: tuple = (0, 0, 0)
    seen: set = field(default_factory=set)
    raw: dict = field(default_factory=dict)  # latest data per notification id

    def update(self, payload):
        for nid, _seq, _last, d in parse_notifications(payload):
            self.seen.add(nid)
            self.raw[nid] = bytes(d)
            if nid == NTF_BUTTONS and d:
                self.buttons = d[0]
            elif nid == NTF_TOUCH and d:
                self.touch = d[0]
            elif nid == NTF_STICK and len(d) >= 4:
                self.stick = struct.unpack_from("<hh", d)
            elif nid == NTF_ANALOG and len(d) >= 3:
                t, g = unpack12(d)
                self.trigger, self.grip = ANALOG_MAX - t, ANALOG_MAX - g
            elif nid == NTF_BATTERY and d:
                self.battery = d[0]
            elif nid == NTF_IMU and len(d) >= 18:
                ax, ay, az, gx, gy, gz = struct.unpack_from("<6h", d, 6)
                self.accel, self.gyro = (ax, ay, az), (gx, gy, gz)


@dataclass
class Calibration:
    stick_full: int = 32000  # observed full deflection ~32500
    deadzone: float = 0.08
    grip_button: int = ANALOG_MAX // 2


def axis(v, cal, invert=False):
    f = max(-1.0, min(1.0, v / cal.stick_full))
    if abs(f) < cal.deadzone:
        return 0
    f = -f if invert else f
    return max(-32768, min(32767, int(f * 32767)))


def trigger(raw):
    return max(0, min(255, raw * 255 // ANALOG_MAX))


def combine(left: Hand, right: Hand, cal=Calibration()):
    """Left Touch = left half of an Xbox pad, right Touch = right half."""
    btn = 0
    if right.buttons & 0x01: btn |= A
    if right.buttons & 0x02: btn |= B
    if left.buttons & 0x01: btn |= X
    if left.buttons & 0x02: btn |= Y
    if left.buttons & 0x04: btn |= LS
    if right.buttons & 0x04: btn |= RS
    if left.buttons & 0x08: btn |= START   # left menu button
    if right.buttons & 0x08: btn |= GUIDE  # right Oculus button (unverified)
    # Grips act as bumpers.
    if left.grip > cal.grip_button: btn |= LB
    if right.grip > cal.grip_button: btn |= RB
    (lx, ly), (rx, ry) = left.stick, right.stick
    return struct.pack(
        "<HBBBhhhh",
        btn,
        8,  # no d-pad on Touch; reserved for a stick-click d-pad mode later
        trigger(left.trigger),
        trigger(right.trigger),
        axis(lx, cal),
        axis(ly, cal, invert=True),  # HID: +y is down
        axis(rx, cal),
        axis(ry, cal, invert=True),
    )


def _selftest():
    payload = (
        encode_notification(NTF_BUTTONS, b"\x15", seq=1)  # X + stick click (+ bit4)
        + encode_notification(NTF_STICK, struct.pack("<hh", 32500, 32500), seq=2)  # up-right
        + encode_notification(NTF_ANALOG, bytes([0x00, 0xF0, 0xFF]), seq=3)  # trigger full, grip released
        + encode_notification(9, bytes(16), seq=4, last=1)
    )
    h = Hand()
    h.update(payload)
    assert h.buttons == 0x15 and (h.trigger, h.grip) == (4095, 0), h
    assert h.stick == (32500, 32500), h.stick
    assert [n for n, *_ in parse_notifications(payload)] == [4, 2, 3, 9]
    btn, hat, lt, rt, lx, ly, rx, ry = struct.unpack("<HBBBhhhh", combine(h, Hand()))
    assert btn == X | LS and lt == 255 and rt == 0 and lx == 32767 and ly == -32767 and rx == ry == 0
    print("mapping selftest ok")


if __name__ == "__main__":
    _selftest()
