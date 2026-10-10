#!/bin/sh
# Build the dongle firmware and flash it: in-application self-update if the bridge
# firmware is running, otherwise USB DFU (needs the RESET button once).
set -e
cd "$(dirname "$0")"
cargo build --release
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy"
"$OBJCOPY" -O binary target/thumbv7em-none-eabihf/release/quest-dongle target/quest-dongle.bin
PY=../host/.venv/bin/python
if [ "$1" != "--dfu" ] && $PY -c "import sys; sys.path.insert(0,'../host'); from bridge import Bridge; Bridge().ping()" 2>/dev/null; then
    $PY ../host/selfupdate.py target/quest-dongle.bin
else
    $PY ../host/dfu.py target/quest-dongle.bin
fi
