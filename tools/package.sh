#!/bin/sh
# Build the firmware and assemble the user download:
#   dist/questpad-<version>.zip = firmware .bin + control panel + launchers + docs.
set -e
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' firmware/Cargo.toml | head -1)
NAME=questpad-$VERSION

(cd firmware && cargo build --release)
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/llvm-objcopy"
"$OBJCOPY" -O binary firmware/target/thumbv7em-none-eabihf/release/questpad firmware/target/questpad.bin

rm -rf "dist/$NAME" && mkdir -p "dist/$NAME/host"
cp firmware/target/questpad.bin "dist/$NAME/questpad.bin"
cp "QuestPad.pyw" "QuestPad.command" README.md LICENSE-MIT LICENSE-APACHE "dist/$NAME/"
cp host/*.py host/requirements.txt "dist/$NAME/host/"
cp -R host/vendor "dist/$NAME/host/vendor"
find "dist/$NAME" -name __pycache__ -prune -exec rm -rf {} +
rm -f "dist/$NAME/host/pairing.json"
(cd dist && rm -f "$NAME.zip" && zip -qr "$NAME.zip" "$NAME")
echo "dist/$NAME.zip"
