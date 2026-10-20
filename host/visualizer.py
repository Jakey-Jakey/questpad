"""3D Controller Visualizer Server for QuestPad.
Serves a WebGL Three.js visualizer over HTTP and streams real-time 3DoF orientation
and input states at 60Hz via Server-Sent Events (SSE).
"""
import json
import math
import os
import struct
import sys
import threading
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

ASSETS_DIR = None
for candidate in (os.path.join(ROOT, "assets"), os.path.join(HERE, "assets"), os.path.join(os.getcwd(), "assets")):
    if os.path.isdir(candidate):
        ASSETS_DIR = candidate
        break
if not ASSETS_DIR:
    ASSETS_DIR = os.path.join(ROOT, "assets")

WEB_DIR = None
for candidate in (os.path.join(HERE, "web"), os.path.join(ROOT, "host", "web"), os.path.join(os.getcwd(), "host", "web"), os.path.join(os.getcwd(), "web")):
    if os.path.isdir(candidate):
        WEB_DIR = candidate
        break
if not WEB_DIR:
    WEB_DIR = os.path.join(HERE, "web")

try:
    import serial  # noqa: F401
except ImportError:
    sys.path.insert(0, os.path.join(HERE, "vendor"))

from ahrs import MadgwickAHRS  # noqa: E402
from bridge import Bridge  # noqa: E402
from mapping import ANALOG_MAX, LOG_ITEM_LEN, Hand  # noqa: E402

HOST = "127.0.0.1"
DEFAULT_PORT = 26762
SIDES = {0x13001101: "left", 0x13011101: "right"}
GYRO_PER_DPS = 8.192
ACCEL_PER_G = 1024.0
DEG_TO_RAD = math.pi / 180.0
STICK_FULL = 32000.0


class ControllerState:
    def __init__(self, side):
        self.side = side
        self.hand = Hand()
        self.ahrs = MadgwickAHRS(sample_period=1.0 / 500.0, beta=0.15)
        self.connected = False
        self.battery = 0
        self.last_seen = 0.0
        self.last_imu_ts = 0
        self.recenter_hold_start = 0.0
        self.recentered = False

    def update_imu(self):
        """Update AHRS orientation filter with latest IMU sample."""
        if not self.hand.imu_ts or self.hand.imu_ts == self.last_imu_ts:
            return
        dt = 0.002
        if self.last_imu_ts > 0:
            diff_us = (self.hand.imu_ts - self.last_imu_ts) & 0xFFFFFFFFFFFF
            if 0 < diff_us < 20000:
                dt = diff_us * 1e-6

        self.last_imu_ts = self.hand.imu_ts
        ax, ay, az = self.hand.accel
        gx, gy, gz = self.hand.gyro

        # Scale to physical units: rad/s for gyro, g for accel
        gx_rad = (gx / GYRO_PER_DPS) * DEG_TO_RAD
        gy_rad = (gy / GYRO_PER_DPS) * DEG_TO_RAD
        gz_rad = (gz / GYRO_PER_DPS) * DEG_TO_RAD
        ax_g = ax / ACCEL_PER_G
        ay_g = ay / ACCEL_PER_G
        az_g = az / ACCEL_PER_G

        # Left controller IMU is mounted rotated 90 degrees relative to right
        if self.side == "left":
            gx_rad, gy_rad = -gy_rad, gx_rad
            ax_g, ay_g = -ay_g, ax_g

        # Align sensor axes with standard Three.js orientation:
        # In right controller: -Z is up, -Y is forward, +X is right.
        # Madgwick filter uses standard convention where reference gravity is +Z.
        # Mapping: gx_m = gx_rad, gy_m = -gz_rad, gz_m = gy_rad
        gx_m = gx_rad
        gy_m = -gz_rad
        gz_m = gy_rad

        ax_m = ax_g
        ay_m = -az_g
        az_m = ay_g

        self.ahrs.update(gx_m, gy_m, gz_m, ax_m, ay_m, az_m, dt=dt)

        # Dual squeeze auto-recenter check: trigger & grip > 85% held for 0.4s
        trig_norm = min(1.0, max(0.0, self.hand.trigger / ANALOG_MAX))
        grip_norm = min(1.0, max(0.0, self.hand.grip / ANALOG_MAX))
        now = time.time()
        if trig_norm > 0.85 and grip_norm > 0.85:
            if self.recenter_hold_start == 0.0:
                self.recenter_hold_start = now
            elif now - self.recenter_hold_start >= 0.4 and not self.recentered:
                self.ahrs.recenter()
                self.recentered = True
        else:
            self.recenter_hold_start = 0.0
            self.recentered = False

    def get_snapshot(self):
        """Returns JSON-serializable state dict for 3D visualizer."""
        now = time.time()
        is_live = self.connected and (now - self.last_seen < 3.0)

        # Stick normalized -1.0 to 1.0 (+y up)
        sx = max(-1.0, min(1.0, self.hand.stick[0] / STICK_FULL))
        sy = max(-1.0, min(1.0, self.hand.stick[1] / STICK_FULL))

        trig = min(1.0, max(0.0, self.hand.trigger / ANALOG_MAX))
        grip = min(1.0, max(0.0, self.hand.grip / ANALOG_MAX))

        # Buttons bitmask: bit0 primary (X or A), bit1 secondary (Y or B),
        # bit2 stick click, bit3 menu/oculus
        btn = self.hand.buttons
        return {
            "connected": is_live,
            "battery": self.battery if self.battery > 0 else self.hand.battery,
            "quat": self.ahrs.get_calibrated_quat(),
            "button_primary": bool(btn & 1),
            "button_secondary": bool(btn & 2),
            "stick_click": bool(btn & 4),
            "menu_or_system": bool(btn & 8),
            "stick": [sx, sy],
            "trigger": trig,
            "grip": grip,
            "touch": self.hand.touch,
        }


def is_valid_ntf(buf):
    if len(buf) < 2:
        return False
    nid = buf[0] & 0x1F
    length = (buf[0] >> 5) | ((buf[1] & 0x0F) << 3)
    return (nid <= 7 or nid == 22) and length > 0 and 2 + length <= len(buf)


class VisualizerServer:
    def __init__(self, host=HOST, port=DEFAULT_PORT, bridge_buzz_fn=None):
        self.host = host
        self.port = port
        self.bridge_buzz_fn = bridge_buzz_fn
        self.ctls = {"left": ControllerState("left"), "right": ControllerState("right")}
        self.slot_to_side = {}
        self.lock = threading.RLock()
        self.running = True
        self.demo_mode = False
        self.demo_start_time = time.time()
        self.subscribers = []
        self.subscribers_lock = threading.Lock()

        self._server = None
        self._start_server()
        threading.Thread(target=self._broadcast_loop, daemon=True).start()

    def set_controller_slot(self, slot, side, battery=None):
        """Called by GUI or frames to associate a slot with a controller hand."""
        with self.lock:
            self.slot_to_side[slot] = side
            ctl = self.ctls[side]
            ctl.connected = True
            ctl.last_seen = time.time()
            if battery is not None:
                ctl.battery = battery

    def _start_server(self):
        handler_cls = self._make_handler()
        for attempt_port in range(self.port, self.port + 20):
            try:
                self._server = ThreadingHTTPServer((self.host, attempt_port), handler_cls)
                self.port = attempt_port
                break
            except OSError:
                continue

        if not self._server:
            raise OSError(f"Could not bind visualizer server on ports {self.port}..{self.port+20}")

        self.url = f"http://{self.host}:{self.port}/"
        threading.Thread(target=self._server.serve_forever, daemon=True).start()

    def _make_handler(self):
        server_instance = self

        class VisualizerHandler(BaseHTTPRequestHandler):
            def log_message(self, format, *args):
                pass  # suppress noisy HTTP access logs

            def do_GET(self):
                path = self.path.split("?")[0]
                if path in ("/", "/index.html"):
                    self._serve_file(os.path.join(WEB_DIR, "index.html"), "text/html; charset=utf-8")
                elif path.startswith("/assets/"):
                    filename = os.path.basename(path)
                    filepath = os.path.join(ASSETS_DIR, filename)
                    if os.path.exists(filepath):
                        self._serve_file(filepath, "model/gltf-binary")
                    else:
                        self.send_error(HTTPStatus.NOT_FOUND)
                elif path == "/events":
                    self._serve_sse()
                elif path == "/status":
                    self._json_reply(server_instance.get_state())
                else:
                    self.send_error(HTTPStatus.NOT_FOUND)

            def do_POST(self):
                path = self.path.split("?")[0]
                if path == "/recenter":
                    side = None
                    if "?" in self.path:
                        for param in self.path.split("?")[1].split("&"):
                            if param.startswith("side="):
                                side = param.split("=")[1]
                    server_instance.recenter(side)
                    self._json_reply({"status": "ok", "recentered": side or "all"})
                elif path == "/toggle_demo":
                    server_instance.demo_mode = not server_instance.demo_mode
                    self._json_reply({"status": "ok", "demo": server_instance.demo_mode})
                else:
                    self.send_error(HTTPStatus.NOT_FOUND)

            def _serve_file(self, path, content_type):
                if not os.path.isfile(path):
                    self.send_error(HTTPStatus.NOT_FOUND)
                    return
                try:
                    with open(path, "rb") as f:
                        data = f.read()
                    self.send_response(HTTPStatus.OK)
                    self.send_header("Content-Type", content_type)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception:
                    pass

            def _json_reply(self, data):
                body = json.dumps(data).encode("utf-8")
                self.send_response(HTTPStatus.OK)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def _serve_sse(self):
                self.send_response(HTTPStatus.OK)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Cache-Control", "no-cache")
                self.send_header("Connection", "keep-alive")
                self.end_headers()

                with server_instance.subscribers_lock:
                    server_instance.subscribers.append(self.wfile)

                # Keep thread alive until client disconnects
                try:
                    while server_instance.running:
                        with server_instance.subscribers_lock:
                            if self.wfile not in server_instance.subscribers:
                                break
                        time.sleep(1.0)
                except Exception:
                    pass
                finally:
                    with server_instance.subscribers_lock:
                        if self.wfile in server_instance.subscribers:
                            server_instance.subscribers.remove(self.wfile)

        return VisualizerHandler

    def recenter(self, side=None):
        with self.lock:
            if side in ("left", "right"):
                self.ctls[side].ahrs.recenter()
            else:
                self.ctls["left"].ahrs.recenter()
                self.ctls["right"].ahrs.recenter()

    def feed(self, f):
        """Accepts frames from Bridge.tap."""
        kind = f[0]
        now = time.time()
        with self.lock:
            # 0xA1: Data notification frame
            if kind == 0xA1 and f[3]:
                slot = f[1]
                side = self.slot_to_side.get(slot)
                p = f[4:]
                if not side or len(p) <= 3 or not (p[2] & 0x40):
                    return
                off = 3 if is_valid_ntf(p[3:]) else 7
                if len(p) <= off:
                    return
                ctl = self.ctls[side]
                ctl.connected = True
                ctl.last_seen = now
                ctl.hand.update(p[off:])
                ctl.update_imu()

            # 0xA0: Link join (controller connected)
            elif kind == 0xA0 and len(f) >= 14:
                dev_type = struct.unpack_from("<I", f, 10)[0]
                side = "right" if (dev_type & 0x1_0000 != 0) else "left"
                slot = f[1]
                self.set_controller_slot(slot, side)
                self.ctls[side].ahrs.recenter()  # Auto-recenter on connect

            # 0xA2: Link drop (controller disconnected)
            elif kind == 0xA2:
                slot = f[1]
                side = self.slot_to_side.pop(slot, None)
                if side:
                    self.ctls[side].connected = False

            # 0xA9: Status reply (slot, side, battery)
            elif kind == 0xA9 and len(f) >= 5:
                status = Bridge.parse_status(f)
                active_sides = set()
                for slot, side, battery in status["controllers"]:
                    self.set_controller_slot(slot, side, battery)
                    active_sides.add(side)
                for side in ("left", "right"):
                    if side not in active_sides:
                        if now - self.ctls[side].last_seen > 4.0:
                            self.ctls[side].connected = False

    def get_state(self):
        with self.lock:
            left_snap = self.ctls["left"].get_snapshot()
            right_snap = self.ctls["right"].get_snapshot()

            # If demo mode is active or both controllers are disconnected and demo enabled
            if self.demo_mode or (not left_snap["connected"] and not right_snap["connected"] and self.demo_mode):
                t = time.time() - self.demo_start_time
                # Synthetic movements matching verto_visualizer.rs demo
                left_snap["connected"] = True
                left_snap["battery"] = 95
                left_snap["button_primary"] = math.sin(t * 4.0) > 0.3
                left_snap["button_secondary"] = math.sin(t * 4.0 + 1.5) > 0.3
                left_snap["menu_or_system"] = math.sin(t * 2.0) > 0.7
                left_snap["stick_click"] = math.cos(t * 3.0) > 0.6
                left_snap["stick"] = [math.cos(t * 2.0) * 0.8, math.sin(t * 2.0) * 0.8]
                left_snap["trigger"] = max(0.0, math.sin(t * 3.0))
                left_snap["grip"] = max(0.0, math.sin(t * 2.5 + 1.0))
                # Euler rotation -> quat
                pitch = math.sin(1.5 * t) * 0.4
                yaw = math.cos(1.3 * t) * 0.4
                roll = math.sin(t * 0.7) * 0.4
                left_snap["quat"] = self._euler_to_quat(pitch, yaw, roll)

                right_snap["connected"] = True
                right_snap["battery"] = 85
                right_snap["button_primary"] = math.sin(t * 4.0 + 2.0) > 0.3
                right_snap["button_secondary"] = math.sin(t * 4.0 + 3.5) > 0.3
                right_snap["menu_or_system"] = math.sin(t * 2.0 + 1.5) > 0.7
                right_snap["stick_click"] = math.cos(t * 3.0 + 1.5) > 0.6
                right_snap["stick"] = [-math.cos(t * 2.0) * 0.8, math.sin(t * 2.5) * 0.8]
                right_snap["trigger"] = max(0.0, math.sin(t * 3.0 + 1.5))
                right_snap["grip"] = max(0.0, math.sin(t * 2.5 + 2.5))
                pitch_r = math.cos(1.5 * t + 0.4) * 0.4
                yaw_r = math.sin(1.3 * t) * 0.4
                roll_r = -math.sin(t * 0.7) * 0.4
                right_snap["quat"] = self._euler_to_quat(pitch_r, yaw_r, roll_r)

            return {
                "demo": self.demo_mode,
                "left": left_snap,
                "right": right_snap,
            }

    def _euler_to_quat(self, pitch, yaw, roll):
        """Converts XYZ Euler angles to Three.js [x, y, z, w] quaternion."""
        cy = math.cos(yaw * 0.5)
        sy = math.sin(yaw * 0.5)
        cp = math.cos(pitch * 0.5)
        sp = math.sin(pitch * 0.5)
        cr = math.cos(roll * 0.5)
        sr = math.sin(roll * 0.5)

        w = cr * cp * cy + sr * sp * sy
        x = sr * cp * cy - cr * sp * sy
        y = cr * sp * cy + sr * cp * sy
        z = cr * cp * sy - sr * sp * cy
        return [x, y, z, w]

    def _broadcast_loop(self):
        """Pushes state to all connected SSE clients at ~60Hz."""
        interval = 1.0 / 60.0
        while self.running:
            t0 = time.time()
            with self.subscribers_lock:
                subs = list(self.subscribers)

            if subs:
                state = self.get_state()
                msg = f"data: {json.dumps(state)}\n\n".encode("utf-8")
                dead = []
                for wfile in subs:
                    try:
                        wfile.write(msg)
                        wfile.flush()
                    except Exception:
                        dead.append(wfile)
                if dead:
                    with self.subscribers_lock:
                        for wfile in dead:
                            if wfile in self.subscribers:
                                self.subscribers.remove(wfile)

            elapsed = time.time() - t0
            sleep_time = interval - elapsed
            if sleep_time > 0:
                time.sleep(sleep_time)

    def close(self):
        self.running = False
        if self._server:
            try:
                self._server.shutdown()
                self._server.server_close()
            except Exception:
                pass


if __name__ == "__main__":
    server = VisualizerServer()
    server.demo_mode = True
    print(f"Visualizer server running at {server.url}")
    print("Press Ctrl+C to exit.")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        server.close()
