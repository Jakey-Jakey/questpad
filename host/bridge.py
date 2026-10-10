"""Host side of the dongle's raw radio bridge (USB CDC, COBS-framed)."""
import struct
import time

import serial
import serial.tools.list_ports

RADIO = 0x40001000
FREQUENCY, TXPOWER, MODE, PCNF0, PCNF1 = 0x508, 0x50C, 0x510, 0x514, 0x518
BASE0, BASE1, PREFIX0, PREFIX1, TXADDRESS, RXADDRESSES = 0x51C, 0x520, 0x524, 0x528, 0x52C, 0x530

# USB ids the dongle firmware uses: HID mode, DS4 mode (XInput mode has no serial port).
DONGLE_IDS = {(0x1209, 0x0001), (0x054C, 0x09CC)}

ADV_BASE = 0xFACEB00C
ADV_PREFIX = 0xAA
ADV_FREQ = 2  # 2402 MHz


def cobs_encode(data):
    out, block = bytearray(), bytearray()
    for b in data:
        if b == 0:
            out += bytes([len(block) + 1]) + block
            block = bytearray()
        else:
            block.append(b)
            if len(block) == 254:
                out += b"\xFF" + block
                block = bytearray()
    out += bytes([len(block) + 1]) + block
    return bytes(out) + b"\x00"


def cobs_decode(data):
    out, i = bytearray(), 0
    while i < len(data):
        code = data[i]
        if code == 0 or i + code > len(data):
            return None
        out += data[i + 1 : i + code]
        i += code
        if code < 0xFF and i < len(data):
            out.append(0)
    return bytes(out)


class Rx:
    def __init__(self, ts, crc_ok, rxmatch, rssi, data):
        self.ts, self.crc_ok, self.rxmatch, self.rssi, self.data = ts, crc_ok, rxmatch, rssi, data

    def __repr__(self):
        return f"Rx(ts={self.ts} crc={'ok' if self.crc_ok else 'BAD'} addr={self.rxmatch} rssi=-{self.rssi} {self.data.hex()})"


class Bridge:
    def __init__(self, port=None):
        if port is None:
            for p in serial.tools.list_ports.comports():
                if (p.vid, p.pid) in DONGLE_IDS:
                    port = p.device
        if port is None:
            raise SystemExit("dongle bridge firmware not found (flash it with firmware/flash.sh)")
        self.s = serial.Serial(port, 115200, timeout=0.05)
        self.acc = bytearray()

    def send(self, payload):
        self.s.write(cobs_encode(payload))

    def frames(self, timeout=None):
        """Yield decoded frames until `timeout` seconds pass (forever if None)."""
        end = None if timeout is None else time.time() + timeout
        while end is None or time.time() < end:
            chunk = self.s.read(4096)
            for b in chunk:
                if b == 0:
                    f = cobs_decode(bytes(self.acc))
                    self.acc.clear()
                    if f:
                        yield f
                else:
                    self.acc.append(b)

    def events(self, timeout=None):
        for f in self.frames(timeout):
            if f[0] == 0x81:
                ts, crc, match, rssi = struct.unpack_from("<IBBB", f, 1)
                yield Rx(ts, bool(crc), match, rssi, f[8:])
            elif f[0] == 0x82:
                yield ("txdone", struct.unpack_from("<I", f, 1)[0])
            else:
                yield f

    def _wait(self, kind, timeout=1.0):
        for f in self.frames(timeout):
            if f[0] == kind:
                return f
        raise TimeoutError(f"no reply {kind:#x}")

    def ping(self):
        self.send(b"\x05")
        f = self._wait(0x84)
        return struct.unpack_from("<I", f, 1)[0], f[5:].decode()

    def poke(self, addr, val):
        self.send(struct.pack("<BII", 0x01, addr, val & 0xFFFFFFFF))

    def peek(self, addr):
        self.send(struct.pack("<BI", 0x07, addr))
        return struct.unpack_from("<I", self._wait(0x87), 5)[0]

    def radio(self, off, val):
        self.poke(RADIO + off, val)

    def stop(self):
        self.send(b"\x03")

    def rx(self):
        self.send(b"\x02")

    def tx(self, raw, rx_after=False):
        self.send(bytes([0x04, int(rx_after)]) + raw)

    def layout(self, len_off, hdr_len, bad_crc=False):
        self.send(bytes([0x06, len_off, hdr_len, int(bad_crc)]))

    def set_address(self, base, prefix, freq, pcnf0, maxlen=255):
        """Single logical address 0 for TX and RX (pipe 0), Pulsar framing."""
        self.stop()
        self.radio(PCNF0, pcnf0)
        self.radio(PCNF1, 0x01040000 | maxlen)
        self.radio(BASE0, base)
        self.radio(BASE1, base)
        self.radio(PREFIX0, prefix)
        self.radio(PREFIX1, 0)
        self.radio(TXADDRESS, 0)
        self.radio(RXADDRESSES, 1)
        self.radio(FREQUENCY, freq)
        if pcnf0 & 0x100:
            self.layout(1, 3)
        else:
            self.layout(0, 1)

    def dfu(self):
        self.send(b"\x7F")

    # ---- DM (device management) host, run by the firmware ----
    def dm_start(self, id0=0, id1=0, any_device=False):
        self.send(struct.pack("<BIIB", 0x10, id0, id1, int(any_device)))

    def dm_stats(self):
        self.send(b"\x12")
        tx, rx, miss, seq = struct.unpack_from("<IIIB", self._wait(0x92), 1)
        return dict(tx=tx, rx=rx, misses=miss, seq=seq)

    def dm_cmd(self, cmd_id, payload=b"", read=False, timeout=2.0):
        """Send SPL command `cmd_id`; returns (error_flag, response_payload)."""
        hdr = (cmd_id << 1) | int(read)
        self.send(bytes([0x11, hdr]) + payload)
        accepted = None
        for f in self.frames(timeout):
            if f[0] == 0x93:
                accepted = f[1]
                if not accepted:
                    raise RuntimeError("dongle rejected DM command (not connected or busy)")
            elif f[0] == 0x91:
                return bool(f[1] & 0x80), f[3:]
            elif f[0] == 0x90 and not f[1]:
                raise RuntimeError("DM link lost")
        raise TimeoutError(f"no response to DM command {cmd_id:#x} (accepted={accepted})")

    def dm_wait_link(self, timeout=None):
        for f in self.frames(timeout):
            if f[0] == 0x90:
                connected, id0, id1 = struct.unpack_from("<BII", f, 1)
                return bool(connected), id0, id1
            if f[0] == 0x81:
                ts, crc, match, rssi = struct.unpack_from("<IBBB", f, 1)
                adv = f[8:]
                print(f"  adv rssi=-{rssi}: {adv.hex(' ')}")
        raise TimeoutError("no controller connected")

    # ---- Pulsar main link host, run by the firmware ----
    LINK_STATS = ("frames", "rx_crc_ok", "rx_crc_bad", "mic_ok", "mic_bad", "joins", "late", "ccm_max_us",
                  "dl_ok", "dl_fail")

    def link_start(self, network_id, link_key, session=0x5155):
        self.send(struct.pack("<BI", 0x20, network_id) + bytes(link_key) + struct.pack("<H", session))

    def link_stats(self):
        self.send(b"\x21")
        return self.parse_link_stats(self._wait(0xA3))

    @classmethod
    def parse_link_stats(cls, f):
        n = (len(f) - 1) // 4
        return dict(zip(cls.LINK_STATS, struct.unpack_from(f"<{n}I", f, 1)))

    def rumble(self, left, right):
        """Haptic amplitude 0..255 per controller (same path as the HID output report)."""
        self.send(bytes([0x25, left, right]))

    def link_request(self, slot, reg, data=b"", read=False):
        """Queue a TL register write/read for the controller in `slot`; True if queued."""
        self.send(bytes([0x26, slot, reg, int(read)]) + bytes(data))
        return bool(self._wait(0x93)[1])

    def pair_store(self, network_id, link_key):
        """Save the pairing in dongle flash (it then hosts the link standalone from power-up)."""
        self.send(struct.pack("<BI", 0x22, network_id) + bytes(link_key))
        f = self._wait(0xF0, timeout=2.0)
        return bool(f[2])

    def pair_get(self):
        """-> (stored network id or None, link running)."""
        self.send(b"\x23")
        f = self._wait(0xA4)
        stored, net, running = struct.unpack_from("<BIB", f, 1)
        return (net if stored else None), bool(running)

    def link_resume(self):
        self.send(b"\x24")

    def link_slots(self):
        """-> {"left"/"right": slot} for the connected controllers."""
        self.send(b"\x27")
        f = self._wait(0xA5)
        return {("right" if f[i + 1] else "left"): f[i] for i in range(1, len(f), 10)}

    # ---- settings / pairing / status (firmware 1.0) ----
    SETTINGS = ("mode", "deadzone_pct", "grip_pct", "idle_min", "off_hold_s", "connect_buzz", "low_batt_pct",
                "single_mode", "gyro_left", "buzz_amp", "led_level", "off_on_suspend", "imu_map_x", "imu_map_y",
                "imu_map_z")
    MODES = ("HID", "DS4", "XInput")
    PAIR_STATES = ("idle", "waiting", "writing", "done", "failed", "timed out", "cancelled")

    def settings_get(self):
        self.send(b"\x29")
        return dict(zip(self.SETTINGS, self._wait(0xA6)[1:]))

    def settings_set(self, values, persist=True):
        """Write settings (dict as from settings_get). Persisting a new mode reboots the dongle
        into it (no reply; reconnect afterwards). Returns True on success."""
        reboot = persist and values["mode"] != self.settings_get()["mode"]
        self.send(bytes([0x2A] + [int(values[k]) & 0xFF for k in self.SETTINGS] + [int(persist)]))
        return True if reboot else bool(self._wait(0xF0, timeout=2.0)[2])

    def status(self):
        """-> dict(mode, paired, pair_state, controllers=[(slot, 'left'/'right', battery or None)])."""
        self.send(b"\x2e")
        f = self._wait(0xA9)
        ctl = [(f[i], "right" if f[i + 1] else "left", None if f[i + 2] == 255 else f[i + 2])
               for i in range(5, 5 + 3 * f[4], 3)]
        return dict(mode=self.MODES[f[1]] if f[1] < 3 else f[1], paired=bool(f[2]),
                    pair_state=self.PAIR_STATES[f[3]] if f[3] < 7 else f[3], controllers=ctl)

    def pair_start(self):
        """Pair a controller from the dongle (or cancel pairing). Progress arrives as 0xA7 frames."""
        self.send(b"\x2b")

    def pair_export(self):
        self.send(b"\x2c")
        f = self._wait(0xA8)
        return (struct.unpack_from("<I", f, 2)[0], f[6:22].hex()) if f[1] else None

    def pair_forget(self):
        self.send(b"\x2d")
