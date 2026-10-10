# Quest Touch Dongle

Use original **Oculus Quest 1 Touch controllers** as a normal USB gamepad — no headset, no phone, no PC software.
A $10 **Nordic nRF52840 Dongle** speaks the controllers' own 2.4 GHz radio protocol, so they connect to it the way
they would to a Quest, and it shows up on your computer as one combined Xbox-style controller with rumble.

> Not affiliated with, endorsed by, or supported by Meta. "Oculus", "Quest" and "Touch" are Meta trademarks, used
> here only to say which hardware this works with.

## Features
- Both controllers as **one gamepad**: left = left stick, X/Y, LT, grip → LB, menu → Start; right = right stick, A/B,
  RT, grip → RB, Oculus → Guide. Stick clicks → LS/RS.
- **Rumble** in both controllers (HID output report).
- **Standalone**: the pairing lives in the dongle; plug it in and turn the controllers on. Works on macOS (and anything
  that accepts a generic HID gamepad: Steam, SDL games, DirectInput on Windows).
- **Power off** the controllers (they have no off switch): hold **Oculus or Menu for 3 s** → short buzz → put them down
  and they switch off once still; they wake when picked up. (Power-off after 10 min idle or when the computer
  sleeps is implemented but not yet verified.)
- Host tools for monitoring, the controllers' debug log, and register access.

Tested with controller firmware **app 1.17.2 / bootloader 1.13.0** on macOS.

## Hardware
- Nordic **nRF52840 Dongle (PCA10059)** with its stock Open USB DFU bootloader.
- Quest 1 Touch controllers (one AA each).

## Setup
Requirements: Rust (stable) with the `thumbv7em-none-eabihf` target, and Python 3.

```sh
rustup target add thumbv7em-none-eabihf
python3 -m venv host/.venv && host/.venv/bin/pip install -r host/requirements.txt
```

**1. First flash** (once): press the dongle's sideways **RESET** button (the red LED pulses), then

```sh
firmware/flash.sh --dfu
```

Later flashes need no button: `firmware/flash.sh` updates the running firmware in place.

**2. Pair each controller** (once per controller). Hold **Oculus + Y** (left) or **Oculus + B** (right) until the
LED blinks, then

```sh
host/.venv/bin/python host/dm.py pair
```

This generates your own network id + link key in `host/pairing.json` (keep it private), writes it to the
controller, and saves it on the dongle. Repeat for the other controller.

**3. Play.** Turn the controllers on; they connect within a second or two.

## Host tools (`host/`)
| tool | what it does |
|---|---|
| `link.py --watch` | live panel of every input, IMU, battery, link stats |
| `link.py --log` | the controllers' own debug log (it rides along in every packet) |
| `rumble.py L R [s]` | test rumble through the HID gamepad |
| `regs.py scan left` | read the controller's registers (versions, battery voltage, IMU config, …) |
| `dm.py info` | device-management info for a controller in pairing mode |

Avoid writing register `0x13` (clears the controller's pairing) and `0x1c` (radio test mode until reset).

## How it works
The controllers talk a proprietary protocol ("Pulsar"): a 2 Mbit nRF radio link with a 2 ms beacon from the host,
BLE-style channel hopping, per-controller time slots and AES-CCM-encrypted uplinks. Pairing uses the controllers'
own device-management mode to install a network id and key that we generate. The full write-up of the protocol,
the dongle internals and everything verified on hardware is in **[NOTES.md](NOTES.md)**; ideas for what's next are
in [IDEAS.md](IDEAS.md).

`research/tools` and `research/ghidra/scripts` contain the analysis helpers used. The firmware images they were run
on (from Meta's Quest 1 OTA) are not included; obtain them yourself if you want to reproduce the research.

## Status / limitations
- USB id is currently the pid.codes **test** PID `1209:0001`; a proper PID is still to be requested.
- No XInput mode yet (Windows XInput games, rumble on Linux); no gyro aim yet. See the Open list in NOTES.md.
- Only Quest 1 Touch controllers have been tested.

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
