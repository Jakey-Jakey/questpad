#!/bin/sh
# Double-click on macOS to open the Quest Touch Dongle control panel.
# Uses a Python with a working Tk (python.org or Homebrew); Apple's built-in Python ships an old Tk
# that hangs on current macOS.
cd "$(dirname "$0")"
for py in host/.venv/bin/python /Library/Frameworks/Python.framework/Versions/Current/bin/python3 \
          /opt/homebrew/bin/python3 /opt/homebrew/bin/python3.1? /usr/local/bin/python3 /usr/local/bin/python3.1? \
          python3; do
    if command -v "$py" >/dev/null 2>&1 &&
       "$py" -c 'import tkinter, sys; sys.exit(0 if tkinter.TkVersion >= 8.6 else 1)' 2>/dev/null; then
        exec "$py" host/gui.py
    fi
done
osascript -e 'display alert "Quest Touch Dongle needs Python" message "Install Python 3 from python.org (it includes Tk), then open this again."'
