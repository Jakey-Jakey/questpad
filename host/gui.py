"""QuestPad control panel: pairing, mode, settings, controller status, firmware.

Run it with any Python 3 that has Tkinter (python.org installers do):
    python3 host/gui.py          (or double-click "QuestPad.pyw" / "QuestPad.command")
Needs nothing else: pySerial is bundled in host/vendor.
"""
import json
import os
import queue
import struct
import sys
import threading
import time
import tkinter as tk
from tkinter import filedialog, messagebox, ttk

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
try:
    import serial  # noqa: F401
except ImportError:
    sys.path.insert(0, os.path.join(HERE, "vendor"))

from bridge import Bridge  # noqa: E402
from mapping import LOG_ITEM_LEN, Hand  # noqa: E402

FIRMWARE_DIRS = [ROOT, os.path.join(ROOT, "firmware"), os.path.join(ROOT, "firmware", "target")]
BOOTLOADER_HELP = {
    False: "1. Plug in the dongle.\n2. Press its small sideways RESET button (the red LED pulses).\n3. Click OK.",
    True: "1. Plug in the nice!nano.\n2. Short its RST pin to GND twice quickly (the blue LED pulses and a "
          "NICENANO drive appears).\n3. Click OK.",
}
MODE_HELP = {
    0: "HID gamepad: works everywhere; Steam asks you to set up the layout once.",
    1: "DS4 (PlayStation 4 controller): Steam, SDL games, macOS and Linux recognise it "
       "instantly, with gyro, rumble and battery. Best choice for Steam.",
    2: "XInput (Xbox 360 controller): native in Windows games, also Linux and macOS. No gyro. "
       "This app can't reach the dongle in this mode: hold the dongle's button 3 s to leave it.",
}
PAIR_TEXT = {
    "idle": "",
    "waiting": "Waiting: hold Oculus + Y (left) or Oculus + B (right) until the controller's LED blinks.",
    "writing": "Pairing...",
    "done": "Paired! The controller is connecting.",
    "failed": "Pairing failed. Try again.",
    "timed out": "No controller found in pairing mode. Try again.",
    "cancelled": "Pairing cancelled.",
}


class Worker(threading.Thread):
    """Owns the serial connection; runs commands from the GUI and streams events back."""

    def __init__(self, events):
        super().__init__(daemon=True)
        self.events = events
        self.cmds = queue.Queue()
        self.bridge = None
        self.paused = False

    def call(self, fn, done=None, fail=None):
        """Run fn(bridge) on the worker; done(result) / fail(error) run on the GUI thread."""
        self.cmds.put((fn, done, fail))

    def _connect(self):
        try:
            self.bridge = Bridge()
            self.events.put(("connected", self.bridge.ping()[1]))
        except (SystemExit, Exception):
            self.bridge = None

    def run(self):
        last_status = 0
        while True:
            if self.bridge is None and not self.paused:
                self._connect()
                if self.bridge is None:
                    self.events.put(("disconnected", None))
            try:
                while True:
                    fn, done, fail = self.cmds.get_nowait()
                    try:
                        result = fn(self.bridge)
                        if done:
                            self.events.put(("call", (done, result)))
                    except BaseException as e:  # noqa: BLE001 - report everything to the GUI
                        if isinstance(e, KeyboardInterrupt):
                            raise
                        if fail:
                            self.events.put(("call", (fail, e)))
                        else:
                            self.events.put(("error", str(e)))
            except queue.Empty:
                pass
            if self.bridge is None:
                time.sleep(0.5)
                continue
            try:
                if time.time() - last_status > 1.0:
                    last_status = time.time()
                    self.events.put(("status", self.bridge.status()))
                for f in self.bridge.frames(0.03):
                    # Live input frames are disposable: never let them pile up if the window stalls.
                    if f[0] != 0xA1 or self.events.qsize() < 200:
                        self.events.put(("frame", f))
            except (SystemExit, Exception):
                try:
                    self.bridge.s.close()
                except Exception:
                    pass
                self.bridge = None
                self.events.put(("disconnected", None))

    def detach(self):
        """Release the port (for firmware updates); `resume` reconnects."""
        self.paused = True
        if self.bridge:
            try:
                self.bridge.s.close()
            except Exception:
                pass
        self.bridge = None

    def resume(self):
        self.paused = False


class App:
    def __init__(self, root, demo=False):
        self.root = root
        root.title("QuestPad")
        root.minsize(560, 0)
        self.events = queue.Queue()
        self.worker = Worker(self.events)
        self.hands = {}  # slot -> (side, Hand)
        self.settings = None
        self.connected = False
        self.version = ""
        self._build()
        if demo:
            self._demo()
            return
        self.worker.start()
        root.after(50, self._poll)

    def _demo(self):
        """Sample state with no dongle (for screenshots and UI work): `gui.py --demo`."""
        self.connected = True
        self._show_settings(dict(mode=1, deadzone_pct=3, grip_pct=50, idle_min=10, off_hold_s=3, connect_buzz=1,
                                 low_batt_pct=10, single_mode=1, gyro_left=0, buzz_amp=200, led_level=40,
                                 off_on_suspend=1, imu_map_x=0, imu_map_y=0x82, imu_map_z=1, touch_left=7,
                                 touch_right=7, touch_both=0, touch_hold_10ms=15, touch_pct=60))
        self._on_status(dict(mode="DS4", paired=True, pair_state="idle",
                             controllers=[(1, "left", 100), (2, "right", 80)]))
        self.ctl_rows["left"][2].set(".Y.. stick  +12, +71  trig   0  grip  86")
        self.ctl_rows["right"][2].set("A... stick   +0,  +0  trig  64  grip   0")

    # ---- layout ----

    def _build(self):
        pad = dict(padx=8, pady=4)
        main = ttk.Frame(self.root, padding=8)
        main.pack(fill="both", expand=True)

        top = ttk.Frame(main)
        top.pack(fill="x", **pad)
        self.status_var = tk.StringVar(value="Looking for the dongle...")
        ttk.Label(top, textvariable=self.status_var, font=("TkDefaultFont", 12, "bold")).pack(side="left")

        # Controllers
        ctl = ttk.LabelFrame(main, text="Controllers", padding=6)
        ctl.pack(fill="x", **pad)
        self.ctl_rows = {}
        for i, side in enumerate(("left", "right")):
            name = ttk.Label(ctl, text=side.capitalize(), width=6)
            name.grid(row=i, column=0, sticky="w")
            state = tk.StringVar(value="not connected")
            ttk.Label(ctl, textvariable=state, width=16).grid(row=i, column=1, sticky="w")
            batt = ttk.Progressbar(ctl, length=80, maximum=100)
            batt.grid(row=i, column=2, padx=6)
            inputs = tk.StringVar(value="")
            ttk.Label(ctl, textvariable=inputs, font=("TkFixedFont", 10), width=34).grid(row=i, column=3, sticky="w")
            ttk.Button(ctl, text="Buzz", width=6, command=lambda s=side: self._buzz(s)).grid(row=i, column=4)
            self.ctl_rows[side] = (state, batt, inputs)
        ttk.Button(ctl, text="Turn off controllers", command=self._power_off).grid(row=2, column=0, columnspan=3,
                                                                                  sticky="w", pady=(6, 0))

        # Pairing
        pr = ttk.LabelFrame(main, text="Pairing", padding=6)
        pr.pack(fill="x", **pad)
        self.pair_btn = ttk.Button(pr, text="Pair a controller", command=self._pair)
        self.pair_btn.grid(row=0, column=0, sticky="w")
        ttk.Button(pr, text="Back up pairing...", command=self._backup).grid(row=0, column=1, padx=4)
        ttk.Button(pr, text="Restore pairing...", command=self._restore).grid(row=0, column=2, padx=4)
        ttk.Button(pr, text="Forget pairing", command=self._forget).grid(row=0, column=3, padx=4)
        self.pair_var = tk.StringVar(value="Or press the dongle's button, then hold Oculus + Y / Oculus + B.")
        ttk.Label(pr, textvariable=self.pair_var, wraplength=520).grid(row=1, column=0, columnspan=4, sticky="w",
                                                                       pady=(4, 0))

        # Mode
        md = ttk.LabelFrame(main, text="Mode", padding=6)
        md.pack(fill="x", **pad)
        self.mode_var = tk.IntVar(value=0)
        for i, name in enumerate(("HID gamepad", "DS4 (Steam, gyro)", "XInput (Xbox 360)")):
            ttk.Radiobutton(md, text=name, value=i, variable=self.mode_var, command=self._mode_help).grid(
                row=0, column=i, sticky="w", padx=4)
        ttk.Button(md, text="Switch", command=self._apply_mode).grid(row=0, column=3, padx=8)
        self.mode_help = tk.StringVar(value=MODE_HELP[0])
        ttk.Label(md, textvariable=self.mode_help, wraplength=520).grid(row=1, column=0, columnspan=4, sticky="w",
                                                                       pady=(4, 0))

        # Settings
        st = ttk.LabelFrame(main, text="Settings", padding=6)
        st.pack(fill="x", **pad)
        self.vars = {}

        def spin(row, col, key, label, lo, hi, step=1):
            ttk.Label(st, text=label).grid(row=row, column=col, sticky="w")
            v = tk.IntVar()
            ttk.Spinbox(st, from_=lo, to=hi, increment=step, textvariable=v, width=5).grid(row=row, column=col + 1, sticky="w",
                                                                          padx=(4, 16))
            self.vars[key] = v

        def check(row, col, key, label):
            v = tk.IntVar()
            ttk.Checkbutton(st, text=label, variable=v).grid(row=row, column=col, columnspan=2, sticky="w")
            self.vars[key] = v

        spin(0, 0, "deadzone_pct", "Stick deadzone %", 0, 30)
        spin(0, 2, "grip_pct", "Grip as bumper at %", 5, 95)
        spin(1, 0, "idle_min", "Turn off after idle (min, 0 = never)", 0, 120)
        spin(1, 2, "off_hold_s", "Hold Oculus/Menu to turn off (s, 0 = off)", 0, 20)
        spin(2, 0, "low_batt_pct", "Low-battery buzz at %", 0, 50)
        spin(2, 2, "buzz_amp", "Feedback buzz strength", 0, 255)
        spin(3, 0, "led_level", "Dongle LED brightness", 0, 255)
        check(4, 0, "connect_buzz", "Buzz when a controller connects")
        check(4, 2, "single_mode", "One controller = whole mini gamepad")
        check(5, 0, "off_on_suspend", "Turn controllers off when the computer sleeps")
        check(5, 2, "gyro_left", "Gyro from the LEFT controller (DS4)")

        # DS4 touchpad: which touch sensors put each thumb on its half of the pad (bits 0 X/A, 1 Y/B, 2 stick,
        # 3 trigger).
        self.touch_vars = {}
        for row, key, side, names in ((6, "touch_left", "Left", ("X", "Y", "stick", "trigger")),
                                      (7, "touch_right", "Right", ("A", "B", "stick", "trigger"))):
            ttk.Label(st, text=f"{side} thumb touches the pad (DS4) on").grid(row=row, column=0, sticky="w")
            boxes = ttk.Frame(st)
            boxes.grid(row=row, column=1, columnspan=3, sticky="w")
            for bit, name in enumerate(names):
                v = tk.IntVar()
                ttk.Checkbutton(boxes, text=name, variable=v).pack(side="left", padx=(4, 8))
                self.touch_vars[key, bit] = v
        check(8, 0, "touch_both", "Both thumbs down = two-finger touch (DS4)")
        spin(8, 2, "touch_hold_ms", "Thumb lift delay (ms)", 0, 1000, step=10)
        spin(9, 0, "touch_pct", "Touch threshold % (lower = more sensitive)", 25, 200, step=5)
        ttk.Button(st, text="Save settings", command=self._save_settings).grid(row=10, column=0, sticky="w",
                                                                                pady=(6, 0))

        # Firmware
        fw = ttk.LabelFrame(main, text="Firmware", padding=6)
        fw.pack(fill="x", **pad)
        ttk.Button(fw, text="Install / update firmware...", command=self._update_firmware).grid(row=0, column=0,
                                                                                            sticky="w")
        self.fw_var = tk.StringVar(value="")
        ttk.Label(fw, textvariable=self.fw_var, wraplength=360).grid(row=0, column=1, sticky="w", padx=8)

    # ---- event pump ----

    def _poll(self):
        try:
            while True:
                kind, data = self.events.get_nowait()
                getattr(self, "_on_" + kind)(data)
        except queue.Empty:
            pass
        self.root.after(50, self._poll)

    def _on_connected(self, version):
        self.connected = True
        self.version = version
        self.status_var.set(f"Connected: {version}")
        self.worker.call(lambda b: b.settings_get(), self._show_settings)

    def _on_disconnected(self, _):
        if self.connected or not self.status_var.get().startswith("Dongle not found"):
            self.status_var.set("Dongle not found. Plug it in (in XInput mode, hold its button 3 s to switch "
                                "back first).")
        self.connected = False
        for state, batt, inputs in self.ctl_rows.values():
            state.set("-")
            batt["value"] = 0
            inputs.set("")

    def _on_error(self, text):
        messagebox.showerror("QuestPad", text)

    def _on_call(self, data):
        fn, value = data
        fn(value)

    def _on_status(self, st):
        mode = st["mode"]
        paired = "paired" if st["paired"] else "not paired yet"
        self.status_var.set(f"Connected - {mode} mode, {paired}")
        seen = set()
        for slot, side, battery in st["controllers"]:
            seen.add(side)
            if slot not in self.hands or self.hands[slot][0] != side:
                self.hands[slot] = (side, Hand())
            state, batt, _ = self.ctl_rows[side]
            flags = st.get("flags", {}).get(slot, 0)
            text = f"connected ({battery}%)" if battery is not None else "connected"
            if flags & Bridge.STATUS_DEAD_BATTERY:
                text = "battery dead, turning off"
            elif flags & Bridge.STATUS_STANDBY:
                text += ", standby"
            state.set(text)
            batt["value"] = battery or 0
        for side, (state, batt, inputs) in self.ctl_rows.items():
            if side not in seen:
                state.set("not connected")
                batt["value"] = 0
                inputs.set("")
        live = {slot for slot, _, _ in st["controllers"]}
        self.hands = {s: h for s, h in self.hands.items() if s in live}
        ps = st["pair_state"]
        if ps in ("waiting", "writing"):
            self.pair_var.set(PAIR_TEXT[ps])
            self.pair_btn.config(text="Cancel pairing")
        else:
            self.pair_btn.config(text="Pair a controller")

    def _on_frame(self, f):
        if f[0] == 0xA7:  # pairing progress
            ps = Bridge.PAIR_STATES[f[1]] if f[1] < len(Bridge.PAIR_STATES) else ""
            self.pair_var.set(PAIR_TEXT.get(ps, ps))
        elif f[0] == 0xA1 and f[3] and f[1] in self.hands:
            p = f[4:]
            if len(p) > 3 + LOG_ITEM_LEN and p[2] & 0x40:
                side, h = self.hands[f[1]]
                h.update(p[3 + LOG_ITEM_LEN:])
                names = "ABS*" if side == "right" else "XYS*"
                btn = "".join(n if h.buttons >> i & 1 else "." for i, n in enumerate(names))
                self.ctl_rows[side][2].set(
                    f"{btn} stick {h.stick[0] // 330:+4d},{h.stick[1] // 330:+4d}  "
                    f"trig {h.trigger * 100 // 4095:3d}  grip {h.grip * 100 // 4095:3d}")

    # ---- actions ----

    def _need(self):
        if not self.connected:
            messagebox.showinfo("QuestPad", "The dongle isn't connected.")
            return False
        return True

    def _buzz(self, side):
        if not self._need():
            return

        def go(b):
            b.rumble(*((200, 0) if side == "left" else (0, 200)))
            time.sleep(0.4)
            b.rumble(0, 0)
        self.worker.call(go)

    def _power_off(self):
        if self._need():
            self.worker.call(lambda b: b.send(b"\x28"))

    def _pair(self):
        if self._need():
            self.pair_var.set(PAIR_TEXT["waiting"])
            self.worker.call(lambda b: b.pair_start())

    def _backup(self):
        if not self._need():
            return

        def save(p):
            if p is None:
                messagebox.showinfo("QuestPad", "Nothing to back up: the dongle isn't paired yet.")
                return
            path = filedialog.asksaveasfilename(title="Save pairing backup", defaultextension=".json",
                                                initialfile="questpad-pairing.json",
                                                filetypes=[("JSON", "*.json")])
            if path:
                with open(path, "w") as fh:
                    json.dump({"network_id": p[0], "link_key": p[1], "controllers": {}}, fh, indent=2)
                messagebox.showinfo("QuestPad", "Saved. Keep this file private: it is your controllers' "
                                    "key.")
        self.worker.call(lambda b: b.pair_export(), save)

    def _restore(self):
        if not self._need():
            return
        path = filedialog.askopenfilename(title="Restore pairing backup", filetypes=[("JSON", "*.json")])
        if not path:
            return
        try:
            p = json.load(open(path))
            net, key = int(p["network_id"]), bytes.fromhex(p["link_key"])
            assert len(key) == 16
        except Exception as e:
            messagebox.showerror("QuestPad", f"Not a pairing backup: {e}")
            return
        self.worker.call(lambda b: b.pair_store(net, key),
                         lambda ok: messagebox.showinfo("QuestPad", "Pairing restored." if ok
                                                        else "Restoring failed."))

    def _forget(self):
        if self._need() and messagebox.askyesno(
                "Forget pairing", "Forget the pairing? Every controller will need pairing again (back it up "
                                  "first if you might want it)."):
            self.worker.call(lambda b: b.pair_forget())

    def _mode_help(self):
        self.mode_help.set(MODE_HELP[self.mode_var.get()])

    def _apply_mode(self):
        if not self._need() or self.settings is None:
            return
        mode = self.mode_var.get()
        if mode == self.settings["mode"]:
            return
        if mode == 2 and not messagebox.askyesno(
                "XInput mode", "In XInput mode this app can't talk to the dongle. To come back, hold the "
                               "dongle's button for 3 seconds (it cycles HID -> DS4 -> XInput). Switch now?"):
            return
        new = dict(self.settings, mode=mode)
        self.worker.call(lambda b: b.settings_set(new))
        self.status_var.set("Switching mode; the dongle is restarting...")

    def _show_settings(self, s):
        self.settings = s
        for k, v in self.vars.items():
            v.set(s.get(k, 0))
        self.vars["touch_hold_ms"].set(s.get("touch_hold_10ms", 15) * 10)
        for (k, bit), v in self.touch_vars.items():
            v.set(s.get(k, 0x07) >> bit & 1)
        self.mode_var.set(s["mode"])
        self._mode_help()

    def _save_settings(self):
        if not self._need() or self.settings is None:
            return
        try:
            new = dict(self.settings, **{k: int(v.get()) for k, v in self.vars.items()})
            new["touch_hold_10ms"] = min(max(round(new.pop("touch_hold_ms") / 10), 0), 255)
            for (k, bit), v in self.touch_vars.items():
                new[k] = new.get(k, 0) & ~(1 << bit) | int(v.get()) << bit
        except (tk.TclError, ValueError):
            messagebox.showerror("QuestPad", "Settings must be whole numbers.")
            return

        def done(ok):
            if ok:
                self.settings = new
                self.status_var.set("Settings saved.")
            else:
                messagebox.showerror("QuestPad", "The dongle didn't accept the settings.")
        self.worker.call(lambda b: b.settings_set(new), done)

    def _update_firmware(self):
        # The dongle takes questpad.bin, the nice!nano questpad-nicenano.uf2 (through its UF2 drive).
        nicenano = self.connected and "nice!nano" in self.version
        name = "questpad-nicenano.uf2" if nicenano else "questpad.bin"
        initial = next((p for p in (os.path.join(d, name) for d in FIRMWARE_DIRS) if os.path.exists(p)), None)
        path = filedialog.askopenfilename(title=f"Choose firmware ({name})",
                                          initialdir=os.path.dirname(initial) if initial else ROOT,
                                          initialfile=os.path.basename(initial) if initial else "",
                                          filetypes=[("Firmware", "*.bin *.uf2")])
        if not path:
            return
        fw = open(path, "rb").read()
        is_uf2 = path.lower().endswith(".uf2")
        connected = self.connected
        if connected and is_uf2 != nicenano:
            messagebox.showerror("Install firmware", f"This {'nice!nano' if nicenano else 'dongle'} needs {name}.")
            return
        if not connected and not messagebox.askokcancel(
                "Install firmware",
                "No board with this firmware is connected, so it must be flashed through its bootloader:\n\n"
                + BOOTLOADER_HELP[is_uf2]):
            return
        progress = lambda text: self.events.put(("call", (self.fw_var.set, text)))  # noqa: E731

        def run():
            import dfu
            import selfupdate
            import uf2
            try:
                if is_uf2:
                    uf2.uf2_flash(fw, progress)
                elif connected:
                    b = Bridge()
                    selfupdate.self_update(fw, progress, b)
                else:
                    dfu.dfu_flash(fw, progress)
                progress("Firmware updated.")
            except Exception as e:  # noqa: BLE001
                progress(f"Update failed: {e}")
            finally:
                time.sleep(2)
                self.worker.resume()

        self.fw_var.set("Starting...")
        self.worker.detach()
        threading.Thread(target=run, daemon=True).start()


def main():
    if tk.TkVersion < 8.6:
        sys.exit("This Python's Tk is too old (macOS's built-in Python hangs). Install Python 3 from python.org.")
    root = tk.Tk()
    App(root, demo="--demo" in sys.argv)
    root.mainloop()


if __name__ == "__main__":
    main()
