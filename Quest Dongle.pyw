# Double-click to open the Quest Touch Dongle control panel (Windows: runs with pythonw, no console).
import os
import runpy
import sys
import traceback

here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(here, "host"))
try:
    runpy.run_path(os.path.join(here, "host", "gui.py"), run_name="__main__")
except SystemExit as e:
    if e.code not in (None, 0):
        import tkinter.messagebox
        tkinter.messagebox.showerror("Quest Touch Dongle", str(e.code))
except Exception:
    # No console under pythonw: show what went wrong instead of silently exiting.
    import tkinter.messagebox
    tkinter.messagebox.showerror("Quest Touch Dongle", traceback.format_exc())
