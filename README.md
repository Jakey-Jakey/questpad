# QuestPad

Use **Meta Quest Touch controllers** as a normal gamepad — no headset, no phone. Quest 1 controllers work today.
A $10 **Nordic nRF52840 Dongle** speaks the controllers' own 2.4 GHz radio protocol, so they connect to it the way
they would to a Quest, and your computer sees one combined controller with rumble (and gyro in DS4 mode).

> Not affiliated with, endorsed by, or supported by Meta, Sony or Microsoft. "Oculus", "Quest" and "Touch" are Meta
> trademarks, used here only to say which hardware this works with.

## Features
- Both controllers as **one gamepad**: left = left stick, X/Y, LT, grip → LB, menu → Start; right = right stick, A/B,
  RT, grip → RB, Oculus → Guide; stick clicks → LS/RS. One controller alone works as a mini gamepad.
- Three USB modes (switch from the app, or hold the dongle's button 3 s):
  | mode | LED | best for |
  |---|---|---|
  | **HID gamepad** | green | anything; Steam asks you to set up the layout once |
  | **DS4** (PlayStation 4 controller) | blue | **Steam**, SDL games, macOS, Linux: no setup, **gyro**, rumble, battery |
  | **XInput** (Xbox 360 controller) | purple | Windows games without Steam; also Linux and macOS |
- **Rumble** in both controllers, a short buzz when one connects, and a buzz when a battery runs low.
- **Pair from the dongle's button**, no computer needed. The pairing is stored on the dongle.
- **Power off** (Quest controllers have no off switch): hold **Oculus or Menu for 3 s** → short buzz → put them down
  and they switch off once still; they wake when picked up. (Switching off after 10 min idle and when the computer
  sleeps is implemented but not yet verified.)
- A small **control panel app** for pairing, modes, settings, battery, test buzz and firmware updates.

## Supported controllers
| controller | status | notes |
|---|---|---|
| **Quest 1** Touch | ✅ working | Tested with controller firmware app 1.17.2 / bootloader 1.13.0, on macOS (all modes) and Windows (HID mode with Steam; DS4 / XInput on Windows not yet verified). |
| **Quest 2** Touch | ❔ untested | Probably a close relative of the same radio protocol, but newer controllers appear to use a key-exchange based pairing that Quest 1 controllers don't, so pairing would need that added. |
| **Quest 3 / 3S** Touch Plus | ❔ untested | Nothing known yet. |

Contributions that make QuestPad work with more controllers are very welcome — see [PROTOCOL.md](PROTOCOL.md) for the
protocol and the tools used to map it.

## What you need
- A Nordic **nRF52840 Dongle (PCA10059)**.
- Quest 1 Touch controllers (one AA each).
- For the control panel: **Python 3** (from [python.org](https://www.python.org/downloads/); it includes Tk).
  Nothing else to install.

## Setup
Download the latest release zip and unzip it.

**1. Install the firmware** (once). Open the control panel (**double-click `QuestPad.pyw`** on Windows,
**`QuestPad.command`** on macOS, or run `python3 host/gui.py`), click **Install / update firmware...**, pick
`questpad.bin`, then press the dongle's small **sideways RESET button** (the red LED pulses) and click OK.
Later updates need no button.

**2. Pair each controller** (once each). Tap the dongle's button (its LED blinks blue) — or click **Pair a
controller** in the app — then hold **Oculus + Y** (left) or **Oculus + B** (right) until the controller's LED
blinks. The dongle flashes green and the controller buzzes when it's done.

**3. Play.** Turn the controllers on (press a button); they connect within a second or two.

**Back up your pairing** from the app (*Back up pairing...*): a new firmware install through the RESET button can
erase it, and restoring it saves re-pairing. Keep the file private; it is your controllers' key.

## The dongle's LED and button
| LED | meaning |
|---|---|
| green / blue / purple, solid | HID / DS4 / XInput mode, controller connected |
| same colour, slow blink | waiting for controllers |
| fast blue blink | pairing |
| red | not paired yet |
| three green / red flashes | pairing worked / failed |

Button: **tap** = pair a controller (tap again to cancel) · **hold 3 s** = next mode (HID → DS4 → XInput) ·
**hold 10 s** = forget the pairing.

In **XInput mode** the control panel can't reach the dongle (Windows' Xbox driver owns it); hold the button 3 s to
switch to HID mode first.

## Settings (control panel)
Stick deadzone, how hard to squeeze a grip for LB/RB, idle power-off time, the power-off hold time, low-battery
warning level, buzz strength, LED brightness, connect buzz, one-controller mini pad, power-off when the computer
sleeps, and which controller's gyro DS4 mode uses.

## For developers
Requirements: Rust (stable) with the `thumbv7em-none-eabihf` target, and Python 3.

```sh
rustup target add thumbv7em-none-eabihf
firmware/flash.sh            # build + flash (in place; --dfu for the RESET-button bootloader)
tools/package.sh             # build the release zip into dist/
```

Command-line tools in `host/` (they use the bundled pySerial; `pip install -r host/requirements.txt` for hidapi):
`link.py --watch` (live inputs, IMU, battery, link stats), `link.py --log` (the controllers' own debug log),
`regs.py scan left` (controller registers), `rumble.py L R [s]`, `dm.py info|pair`.
Avoid writing controller register `0x13` (clears its pairing) and `0x1c` (radio test mode until reset).

## How it works
The controllers talk a proprietary protocol ("Pulsar"): a 2 Mbit nRF radio link with a 2 ms beacon from the host,
BLE-style channel hopping, per-controller time slots and AES-CCM-encrypted uplinks. Pairing uses the controllers'
own device-management mode to install a network id and key that the dongle generates. The protocol write-up is in
**[PROTOCOL.md](PROTOCOL.md)**; ideas for what's next are in [IDEAS.md](IDEAS.md).

`research/tools` and `research/ghidra/scripts` contain the analysis helpers used. The firmware images they were run
on (from Meta's Quest 1 OTA) are not included.

## Limitations
- HID mode uses the pid.codes **test** USB id `1209:0001` until a proper one is assigned.
- Gyro orientation was measured on the right controller; using the left one's gyro is unverified.
- Only Quest 1 Touch controllers have been tested.

## License
Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
Includes pySerial (BSD-3-Clause, `host/vendor/serial/LICENSE.txt`) and USB descriptors adapted from GP2040-CE (MIT,
noted in `firmware/src/ds4.rs` and `firmware/src/xinput.rs`).
