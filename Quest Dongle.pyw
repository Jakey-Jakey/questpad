# Double-click to open the Quest Touch Dongle control panel (Windows: runs with pythonw, no console).
import os
import runpy
import sys

here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(here, "host"))
runpy.run_path(os.path.join(here, "host", "gui.py"), run_name="__main__")
