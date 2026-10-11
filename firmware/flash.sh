#!/bin/sh
# Build the dongle firmware and flash it: in-application self-update if the bridge
# firmware is running, otherwise USB DFU (needs the RESET button once).
set -e
cd "$(dirname "$0")"
cargo build --release
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy"
"$OBJCOPY" -O binary target/thumbv7em-none-eabihf/release/questpad target/questpad.bin
# Python: the host venv if there is one (bin/ on macOS / Linux, Scripts/ on Windows), else the
# first python3 / python that runs (Windows' python3 can be a Store stub), with the bundled pySerial.
for PY in ../host/.venv/bin/python ../host/.venv/Scripts/python python3 python ""; do
    [ -n "$PY" ] && "$PY" -c "" 2>/dev/null && break
done
[ -n "$PY" ] || { echo "Python 3 not found" >&2; exit 1; }
export PYTHONPATH=../host/vendor
if [ "$1" != "--dfu" ] && $PY -c "import sys; sys.path.insert(0,'../host'); from bridge import Bridge; Bridge().ping()" 2>/dev/null; then
    $PY ../host/selfupdate.py target/questpad.bin
else
    $PY ../host/dfu.py target/questpad.bin
fi
