#!/bin/sh
# Build the firmware and assemble the user download:
#   dist/questpad-<version>.zip = firmware (dongle .bin, nice!nano .uf2) + control panel +
#   launchers + docs.
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' firmware/Cargo.toml | head -1)
NAME=questpad-$VERSION

for PY in python3 python; do "$PY" -c "" 2>/dev/null && break; done
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy"
ELF=firmware/target/thumbv7em-none-eabihf/release/questpad
(cd firmware && cargo build --release)
"$OBJCOPY" -O binary "$ELF" firmware/target/questpad.bin
(cd firmware && cargo build --release --features nicenano)
"$OBJCOPY" -O binary "$ELF" firmware/target/questpad-nicenano.bin
"$PY" host/uf2.py firmware/target/questpad-nicenano.bin -o firmware/target/questpad-nicenano.uf2

rm -rf "dist/$NAME" && mkdir -p "dist/$NAME/host"
cp firmware/target/questpad.bin firmware/target/questpad-nicenano.uf2 "dist/$NAME/"
cp "QuestPad.pyw" "QuestPad.command" README.md LICENSE-MIT LICENSE-APACHE "dist/$NAME/"
cp host/*.py host/requirements.txt "dist/$NAME/host/"
cp -R host/vendor "dist/$NAME/host/vendor"
find "dist/$NAME" -name __pycache__ -prune -exec rm -rf {} +
rm -f "dist/$NAME/host/pairing.json"
# zip if installed (Git Bash on Windows has none), else Python's zipfile.
cd dist && rm -f "$NAME.zip"
if command -v zip >/dev/null; then
    zip -qr "$NAME.zip" "$NAME"
else
    "$PY" -m zipfile -c "$NAME.zip" "$NAME"
fi
cd ..
echo "dist/$NAME.zip"
