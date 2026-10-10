# Ideas / explorations

Loose ideas for what QuestPad could do next. The protocol and register map are in PROTOCOL.md.

## Motion (IMU: 500 Hz accel + gyro in every uplink, ntf id 1)
- **Air mouse / couch-remote mode**: point to move the cursor, trigger = click, stick = scroll. Dongle adds a HID
  mouse interface; toggle with a button combo.
- **Tilt steering**: roll angle → a stick axis (Wii-wheel style).
- **Two-hand "invisible wheel"**: hold both like a steering wheel; turning rotates both controllers the same way
  around the wheel axis → average of their rolls (gravity-referenced, drift-free) = steering. Each IMU sees only its
  own rotation, not position, so "push one hand forward, pull the other back" can't be measured directly (double-
  integrating the accelerometer drifts within a second); it shows up only as the yaw it causes.
- **Two-hand gestures**: both controllers' orientations together (e.g. both pitched up = brace / shield, mirrored
  roll = lean) — orientation is reliable, relative position is not (needs camera / hand-tracking, see IR + Steam Frame).
- **Gestures**: shake / flick / punch detection mapped to buttons (e.g. flick to reload).
- **Find my controller**: buzz a controller from the other one or from the Mac; or buzz whichever was just jostled
  (motion wakes them anyway).

## Touch (capacitive bits, ntf id 5)
- **Hands-on detection**: pause / unpause or enable features based on whether the controller is actually held.
- **Touch as extra inputs**: resting on a button ≠ pressing it (e.g. rest on X = show overlay, press = select).

## Haptics
- **Waveform effects** via the sample-buffer registers 0x96 / 0x9b (ticks, ramps, heartbeats) instead of the single
  amplitude in 0x97. Needs the 32-byte message format decoded (`haptic_manager.c FUN_00013c84` / `FUN_00013a54`).
- **Audio → haptics**: Mac audio level / bass drives rumble.
- **Notification buzzes**: small menu-bar app that buzzes for calendar alerts, messages, build-finished, etc.

## Battery
- Runtime estimate from the AA voltage curve (reg 0x2f mV) logged over time.

## Power
- Auto-standby (reg 0x06) whenever no app is using the gamepad, for battery life.
- Re-pair from the computer: reg 0x15 reboots a controller into DM / pairing mode (untested), skipping HOME+B.

## IR LED ring (reg 0x28: period 33333/40000 µs, pulse 1..99 µs)
- **Camera tracking**: an IR-capable webcam (or one with its IR filter removed) sees the ring. We control flash
  timing, so we could sync flashes to the webcam's exposure. "See the dots" is easy; full optical 6DoF (constellation
  tracking fused with our IMU) is a big project.

## Making it accessible (no Rust / Python for users)
Everything host-side goes over the dongle's USB CDC serial port (Nordic DFU = SLIP + protobuf init packet, our
self-update, DM pairing, bridge commands), so it can all move out of the Python scripts.
- **Web app (Web Serial)**: static page on GitHub Pages (HTTPS, required). Flash: bootloader → Nordic serial DFU
  (port `dfu.py`; show "press RESET" with a picture, the only button press ever needed), running firmware →
  in-place self-update (port `selfupdate.py`). Pair: generate network id + key with `crypto.getRandomValues`,
  guide "hold Oculus + Y/B", run DM pairing (`dm.py`), store on the dongle (bridge 0x22), offer a backup download.
  Test / configure: live input panel, rumble, battery, power off, future settings. ~600–800 lines of JS.
  Limits: Chromium desktop only (no Safari / Firefox); Linux needs serial permission (`dialout`). Firmware .bin is
  published with the site by a GitHub Action (browsers can't fetch GitHub release assets: no CORS).
- **Desktop app (BetterJoy-style, lightweight)**: tray app with battery, connect / disconnect notifications, power-off,
  settings, flash + pair. Unlike BetterJoy it doesn't need to do the mapping (the firmware already presents a gamepad),
  so it can stay small. What it adds over the web app: always running (battery warnings, notification buzzes,
  audio → haptics, per-game profile switching), works in any browser setup, and can create **virtual controllers**
  (ViGEm on Windows → XInput without a firmware XInput mode; `uinput` on Linux). Costs: three platform builds,
  code signing / notarization (macOS Gatekeeper, Windows SmartScreen warnings without it).
- **Both from one codebase**: build the UI as a web app with a small "transport" interface (Web Serial in the
  browser, native serial in a **Tauri** wrapper). Ship the web page first (zero install for flashing / pairing), then
  wrap it as the tray app.

## Status / tooling
- `quest-status`: serials, firmware versions, battery % + mV, RSSI, packet loss, link health per controller.
- Decode calibration registers (0x4c / 0x4f / 0x99) so stick centre / trigger ranges use factory values.

## Steam Frame / SteamVR
Assumptions about the headset are unverified (check current docs first): standalone SteamOS headset that can also
stream from a PC.
1. **Gamepad** (should work today): dongle is a standard USB HID gamepad → Steam Input on the headset or the PC.
   Rumble likely needs XInput mode (Linux `xpad` supports it; generic HID output probably isn't used).
2. **Rotation-only (3DoF) VR controllers**: SteamVR/OpenVR driver on the PC (streaming setup) reading the dongle →
   two controllers with IMU orientation, buttons, triggers, sticks, touch, haptics; position from an arm model
   (Go / Daydream style). Running it on the headset depends on whether its SteamOS allows custom drivers.
3. **Position (6DoF)**: Frame camera access unlikely. Options: fuse headset hand-tracking (hand position) with our
   IMU (orientation); or an external IR webcam + LED-timing control (see IR section).

## Bluetooth
- Standard-Bluetooth controllers would need replacement controller firmware
- Dongle BLE bridge mode: dongle on a charger / power bank bridges the Pulsar link to a phone / PC as a BLE HID gamepad; the one radio is time-shared (skip Pulsar frames for BLE events; controllers tolerate 24 missed beacons).
