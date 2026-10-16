#!/bin/sh
# Build the dongle firmware and flash it: in-application self-update if the bridge
# firmware is running, otherwise USB DFU (needs the RESET button once).
#   --dfu       always use the DFU bootloader
#   --nicenano  nice!nano v2 build, installed through its UF2 bootloader
set -e
cd "$(dirname "$0")"
DFU= NICENANO=
for a in "$@"; do
    case "$a" in
        --dfu) DFU=1 ;;
        --nicenano) NICENANO=1 ;;
        *) echo "usage: flash.sh [--dfu] [--nicenano]" >&2; exit 2 ;;
    esac
done
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy"
# Python: the host venv if there is one (bin/ on macOS / Linux, Scripts/ on Windows), else the
# first python3 / python that runs (Windows' python3 can be a Store stub), with the bundled pySerial.
for PY in ../host/.venv/bin/python ../host/.venv/Scripts/python python3 python ""; do
    [ -n "$PY" ] && "$PY" -c "" 2>/dev/null && break
done
[ -n "$PY" ] || { echo "Python 3 not found" >&2; exit 1; }
export PYTHONPATH=../host/vendor
if [ -n "$NICENANO" ]; then
    cargo build --release --features nicenano
    "$OBJCOPY" -O binary target/thumbv7em-none-eabihf/release/questpad target/questpad-nicenano.bin
    $PY ../host/uf2.py target/questpad-nicenano.bin -o target/questpad-nicenano.uf2
    exec $PY ../host/uf2.py target/questpad-nicenano.uf2
fi
cargo build --release
"$OBJCOPY" -O binary target/thumbv7em-none-eabihf/release/questpad target/questpad.bin
if [ -z "$DFU" ] && $PY -c "import sys; sys.path.insert(0,'../host'); from bridge import Bridge; Bridge().ping()" 2>/dev/null; then
    $PY ../host/selfupdate.py target/questpad.bin
else
    $PY ../host/dfu.py target/questpad.bin
fi
