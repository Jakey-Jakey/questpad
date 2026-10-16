"""Flash the nice!nano v2 through its UF2 bootloader (Adafruit's: a USB drive called NICENANO).

Usage: uf2.py <firmware.uf2 | firmware.bin>     install (a .bin is converted first)
       uf2.py <firmware.bin> -o <firmware.uf2>  only convert

With QuestPad running (HID or DS4 mode), the board reboots into its bootloader on its own.
Otherwise double-tap RST to GND: the blue LED pulses and the NICENANO drive appears.
"""
import argparse
import glob
import os
import string
import struct
import sys
import time

APP_ADDR = 0x26000  # above the S140 SoftDevice the bootloader keeps (firmware/memory-nicenano.x)
FAMILY = 0xADA52840  # nRF52840 application
BOARD_ID = "nRF52840-nicenano"  # in INFO_UF2.TXT on the bootloader's drive
PAYLOAD = 256


def from_bin(fw, addr=APP_ADDR):
    """UF2 image of `fw`, a flat binary that starts at `addr`."""
    n = (len(fw) + PAYLOAD - 1) // PAYLOAD
    out = bytearray()
    for i in range(n):
        data = fw[i * PAYLOAD : (i + 1) * PAYLOAD].ljust(PAYLOAD, b"\xFF")
        # magic, magic, flags (family id present), address, payload size, block, blocks, family
        out += struct.pack("<8I", 0x0A324655, 0x9E5D5157, 0x2000, addr + i * PAYLOAD, PAYLOAD, i, n, FAMILY)
        out += data.ljust(476, b"\0") + struct.pack("<I", 0x0AB16F30)
    return bytes(out)


def _mounts():
    if sys.platform == "win32":
        import ctypes

        k32 = ctypes.windll.kernel32
        letters = [c for i, c in enumerate(string.ascii_uppercase) if k32.GetLogicalDrives() >> i & 1]
        return [f"{c}:\\" for c in letters if k32.GetDriveTypeW(f"{c}:\\") == 2]  # DRIVE_REMOVABLE
    if sys.platform == "darwin":
        return glob.glob("/Volumes/*")
    return glob.glob("/media/*/*") + glob.glob("/run/media/*/*") + glob.glob("/media/*")


def find_drive():
    """Mount point of a nice!nano bootloader drive, or None."""
    for d in _mounts():
        try:
            with open(os.path.join(d, "INFO_UF2.TXT")) as f:
                if BOARD_ID in f.read():
                    return d
        except OSError:
            pass
    return None


class Uf2Error(Exception):
    pass


def uf2_flash(image, progress=print, bridge=None):
    """Install `image` (UF2 bytes). A running QuestPad (`bridge`, else found) reboots into the
    bootloader first. Returns the new firmware's version, or None if the board isn't reachable."""
    from bridge import Bridge

    drive = find_drive()
    if not drive:
        try:
            b = bridge or Bridge()
            if "nice!nano" in b.ping()[1]:  # not a PCA10059 dongle plugged in alongside
                progress("rebooting the board into its bootloader")
                b.send(b"\x7F")  # CMD_DFU
            b.s.close()
        except (SystemExit, Exception):
            pass  # not running QuestPad: wait for a double-tap on RST
        for _ in range(75):
            time.sleep(0.2)
            drive = find_drive()
            if drive:
                break
    if not drive:
        raise Uf2Error("No NICENANO drive found. Double-tap the nice!nano's RST pin to GND (the blue LED "
                       "pulses and the drive appears), then retry.")
    progress(f"copying {len(image)} bytes to {drive}")
    err = None
    try:
        with open(os.path.join(drive, "QUESTPAD.UF2"), "wb") as f:
            f.write(image)
            f.flush()
            os.fsync(f.fileno())
    except OSError as e:
        err = e  # fine if the drive went away: the board resets once the last block lands
    for _ in range(50):
        if not os.path.exists(os.path.join(drive, "INFO_UF2.TXT")):
            break
        time.sleep(0.2)
    else:
        raise Uf2Error(f"the bootloader didn't take the file ({err or 'its drive is still there'})")
    progress("restarting into the new firmware")
    for _ in range(30):
        time.sleep(0.5)
        try:
            version = Bridge().ping()[1]
            progress(f"board: {version}")
            return version
        except (SystemExit, Exception):
            pass
    progress("installed (in XInput mode the board has no serial port to check it)")
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("firmware", help=".uf2, or a .bin that starts at 0x26000")
    ap.add_argument("-o", "--output", help="write the .uf2 here instead of installing it")
    args = ap.parse_args()
    data = open(args.firmware, "rb").read()
    image = data if args.firmware.lower().endswith(".uf2") else from_bin(data)
    if args.output:
        with open(args.output, "wb") as f:
            f.write(image)
        return
    try:
        uf2_flash(image)
    except Uf2Error as e:
        sys.exit(str(e))


if __name__ == "__main__":
    main()
