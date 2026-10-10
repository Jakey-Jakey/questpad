"""Capture the control panel for the README (macOS): docs/gui.png.

  python3 tools/screenshot.py          sample data (gui.py --demo), no dongle needed
  python3 tools/screenshot.py --live   with the real dongle connected
"""
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "host"))
import gui  # noqa: E402
import tkinter as tk  # noqa: E402

live = "--live" in sys.argv
root = tk.Tk()
app = gui.App(root, demo=not live)
root.geometry("+80+80")
root.attributes("-topmost", True)
root.focus_force()
end = time.time() + (5 if live else 3)
while time.time() < end:
    root.update_idletasks()
    root.update()
    time.sleep(0.03)
x, y, w, h = root.winfo_rootx(), root.winfo_rooty(), root.winfo_width(), root.winfo_height()
TITLE_BAR = 27  # points, macOS
subprocess.run(["screencapture", "-x", f"-R{x},{y - TITLE_BAR},{w},{h + TITLE_BAR}",
                os.path.join(ROOT, "docs", "gui.png")], check=True)
root.destroy()
