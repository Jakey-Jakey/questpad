# Ideas / explorations

Loose ideas for what the dongle + controller registers enable. Committed work lives in NOTES.md → Open;
facts (register map, protocol) live in NOTES.md too. Items here are unscoped until promoted.

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
