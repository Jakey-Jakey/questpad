# How QuestPad works

This is a guide to the Quest 1 Touch controllers' radio protocol as QuestPad implements it, and to the dongle
firmware: enough to understand the code, debug it, or add support for another controller. Everything here was
verified on hardware (controller firmware 1.17.2, bootloader 1.13.0). Multi-byte values are little-endian unless
noted. Meta's internal name for the protocol is **Pulsar**.

- [At a glance](#at-a-glance)
- [Radio](#radio)
- [Pairing](#pairing)
- [The main link](#the-main-link)
- [Controller data](#controller-data)
- [Commands to a controller](#commands-to-a-controller)
- [Controller registers](#controller-registers)
- [The dongle firmware](#the-dongle-firmware)
- [Adding another controller](#adding-another-controller)

## At a glance
| | |
|---|---|
| Radio | Nordic proprietary 2 Mbit/s, 2404–2478 MHz, 24-bit CRC |
| Pairing | the controller's bootloader accepts a network id + 128-bit key over a separate "device management" link |
| Link timing | 2 ms frames: one beacon from the host, then up to five controller time slots |
| Hopping | 37 channels, BLE channel-selection algorithm #1, hop size from the network id |
| Security | controller → host packets are AES-CCM encrypted with the pairing key; beacons are plaintext |
| Controller data | a 4-byte debug-log chunk plus tagged entries: buttons, stick, trigger/grip, touch, IMU, battery |
| Commands | register reads / writes carried in beacons: vibration, power off, standby, … |

## Radio
QuestPad drives the nRF52840 radio directly with the same settings the headset uses:

| setting | value |
|---|---|
| mode | `MODE = 1` (Nordic 2 Mbit), fast ramp-up, no whitening |
| address | 4-byte base + 1-byte prefix, big-endian on air (`PCNF1 = 0x01040000 \| maxlen`) |
| CRC | 24 bits, covers the address: `CRCCNF = 3`, poly `0x1108421`, init `0xFFFFFF` |
| packet layout | main link `[S0][LEN][S1][payload]` (`PCNF0 = 0x00100108`); pairing `[LEN][payload]` (`PCNF0 = 8`) |

## Pairing
Holding **Oculus + Y** (left) or **Oculus + B** (right) starts the controller's bootloader in a pairing mode
("device management", DM).

**Advertising.** The controller advertises every 102.2 ms for about a minute on 2402 MHz, base address
`0xFACEB00C`, prefix `0xAA`:

| bytes | content |
|---|---|
| 0 | format version (2) |
| 1..5 | type: `0x13001101` left, `0x13011101` right (bit 16 = hand) |
| 5..9, 9..13 | `id0`, `id1` |
| 13..17 | unknown |
| 17..31 | serial number, 14 ASCII characters |
| 31 | status bits |

**Connecting.** The host answers on 2426 MHz with base address `id0` (prefix `0xAA`), 1000 µs after the end of an
advertisement. From then on the two sides alternate, each transmitting 1000 µs after the end of the other's packet.

**Commands.** Host packet `[LEN][hdr][seq][payload]` with `hdr = command << 1 | read`; replies echo `seq`, and bit 7
of the reply header marks an error. Empty polls `[2][0][seq]` keep the link alive.

| command | what |
|---|---|
| `0x03` read | serial number |
| `0x24` / `0x25` read | app / bootloader version |
| `0x2F` read | battery |
| `0x14` write | **pairing record**: `network_id u32`, `link_key[16]` |
| `0x15` write | reset into the main app |

Quest 1 controllers take the pairing record in plaintext; newer controllers appear to use a key-exchange based
pairing instead. QuestPad generates a random network id (one that makes a good radio address: plenty of bit
transitions, no long runs of equal bits) and a random 128-bit key, writes them to each controller, and keeps them in
the dongle's flash.

## The main link
The dongle plays the headset's role. Time is divided into **2000 µs frames**:

```
 T                 T+350  T+575   T+875   T+1175  T+1475           T+2000
 |  beacon (host)  | slot0 | slot1 | slot2 | slot3 | slot4 |  idle   | next beacon ...
 |<-- ~120 µs ---->|         controllers transmit (~250 µs each)
```

**Addressing.** Base address = `network_id`. The beacon uses prefix `0xF0`; a controller in slot *s* uses prefix
*s* + 1, so the host listens on logical addresses 1..5 and `RXMATCH − 1` is the slot. Slot 0 is shared, for joining.

**Channels.** 37 channels, the Bluetooth LE data-channel set: index 0..36 → `FREQUENCY` 4, 6, …, 24, 28, …, 78
(2404–2478 MHz, skipping 2426). Each frame the host hops with BLE channel-selection algorithm #1:
`unmapped = (current + hop) % 37` (remapped through the list of used channels if masked), with
`hop = (network_id & 0xFF) % 11 + 5`. A controller looking for its host listens on channel indices 0, 17 and 36 in
turn; once it hears a beacon it follows the hopping, and gives up after 24 missed beacons.

**Beacon** (`S0 = 4`, not encrypted):

| bytes | content |
|---|---|
| 0..5 | 3 flag bits, then the 37-bit channel map shifted left by 3 |
| 5 | this frame's (unmapped) channel index |
| 6..8 | session id (any value; part of the encryption IV) |
| 8..14 | 48-bit timestamp in µs (part of the IV; must advance by exactly 2000 per frame) |
| 14.. | control data, below |

**Control data** in the beacon is normally `[downlink mask][ack mask]`, where bit *s* of the ack mask means "I got
your slot-*s* packet last frame" (QuestPad acks on a good CRC, like the headset).

**Joining.** A new controller sends `[0][1][device_id, 8 B][type u32]` in slot 0. The host answers in the next
beacon's control data with `[1][ack mask][2][device_id][slot 1..4][0][1]`, padded to 36 bytes (`3` instead of `2`
rejects). The controller then transmits in its own slot every frame.

**Encryption.** Controller packets are AES-CCM encrypted (the nRF CCM peripheral in BLE-style, extended-length,
2 Mbit mode; 4-byte MIC) with the pairing key, packet counter 0, direction 0, and an 8-byte IV made of the beacon
timestamp (48 bits) and the session id (16 bits). The nRF CCM decrypts at roughly radio speed (about 310 µs for a
full packet), so the firmware queues packets and finishes them in the CCM interrupt rather than waiting.

## Controller data
Each decrypted controller packet looks like:

```
[slot] [0x01] [0x40 | seq] [log chunk, 4 B] [entry] [entry] ...
```

The 4-byte log chunk carries the controller's own text debug log a few characters at a time (`host/link.py --log`
prints it). It only advances when the host acks the packet.

Each **entry** is `[id:5 | len_lo:3 << 5] [len_hi:4 | seq:3 << 4 | last << 7] [data]`:

| id | content |
|---|---|
| 0 | battery % |
| 1 | IMU, 18 B: timestamp (48-bit µs), accel x y z (i16, ~1024 per g), gyro x y z (i16, ~8.2 per °/s); every 2 ms |
| 2 | thumbstick x, y (i16; +x right, +y up; exactly 0, 0 at rest, already calibrated) |
| 3 | trigger, grip: two 12-bit values packed in 3 bytes, inverted (4095 = released, 0 = fully squeezed) |
| 4 | buttons: bit 0 X / A, bit 1 Y / B, bit 2 stick click, bit 3 Menu / Oculus (bit 4 is always set) |
| 5 | capacitive touch: 0x01 X / A, 0x02 Y / B, 0x04 stick, 0x08 trigger (finger on it); 0x10 / 0x20 / 0x40 / 0x80 = finger near the same four |
| 5 thresholds | the controller sets touch above 25 / 20 / 45 / 100 and near above 18 / 15 / 25 / 50 (X / A, Y / B, stick, trigger) on the raw levels in id 7 |
| 6 | status flags, sent when the controller changes state: 0x01 dead battery (it shuts down right after), 0x10 standby (register 0x06) |
| 7 | raw touch levels, 4 × u8 (X / A, Y / B, stick, trigger), clamped to 0..255; ~0 untouched, ~200 for a finger on the trigger, 30..60 hovering |

**IMU orientation** (right controller, held naturally): −Z points up and −Y points forward, so for a DS4-style frame
(x right, y up, z toward you) DS4 x = +X, y = −Z, z = +Y.

## Commands to a controller
A beacon can carry one request, addressed by its downlink mask:

```
[1 << slot] [ack mask] [register] [seq | read << 4] [data, up to 32 B]
```

A controller acts on a request when its seq (or register, or read bit) differs from the last request it took, and
reports the seq of the last request frame it saw in the low nibble of its packet flags. Every controller reports the
seq of *any* request it saw, so QuestPad uses one seq counter for all controllers and repeats a request until the
target controller reports its seq. Read replies come back as `[slot][register][flags][data]` without the `0x40`
bit; flags bit 5 means error.

## Controller registers
`host/regs.py` reads and writes these. Many other register numbers exist but do nothing on this firmware.

| register | | what |
|---|---|---|
| `0x01` | read | maker / product / hand / hardware revision strings |
| `0x02`, `0x03` | read | board serial, device serial |
| `0x24`, `0x25` | read | app / bootloader version |
| `0x2F` | read | battery: f32 %, then the cell voltage (u16, mV) |
| `0x31`, `0x32` | read | IMU part number and configuration |
| `0x05` | write | **power off** (wakes on motion or a button) |
| `0x06` | write | `1` standby (packets almost stop), `0` active |
| `0x97` | write | **vibration strength**, 1 byte; stays on until set back to 0 |
| `0x28` | read / write | IR LED timing: period (33333 or 40000 µs) and pulse width (1–99 µs) |
| `0x15` / `0x16` | write | restart the controller (probably into pairing mode / the main app; untested) |
| `0x13` | write | ⚠️ clears the controller's pairing |
| `0x1c` | write | ⚠️ radio test mode until the controller is reset |

## The dongle firmware
Rust on [embassy](https://embassy.dev), for the nRF52840 Dongle (PCA10059). The application sits at `0x1000`, under
Nordic's Open USB DFU bootloader.

| | |
|---|---|
| `link.rs` | the main-link host: frame timing, hopping, joins, decryption queue, requests |
| `radio.rs` | radio driver and the pairing-mode (DM) engine |
| `gamepad.rs` | controller data → one pad state; HID mode report; power-off gesture and idle timer |
| `ds4.rs`, `xinput.rs` | DS4 and Xbox 360 modes (descriptor / layout from GP2040-CE) |
| `ui.rs` | status LED, button, pairing from the dongle |
| `settings.rs`, `pairing.rs` | settings (flash `0xDE000`) and the pairing record (flash `0xDF000`) |
| `selfupdate.rs`, `wdt.rs` | in-place firmware updates, watchdog |
| `main.rs` | USB setup per mode, and the serial protocol the control panel uses (command list at the top) |

**USB modes.** Generic gamepad + serial port (`1209:5151`; firmware 1.0.0 used the pid.codes test id `1209:0001`), DS4 (`054C:09CC`, gamepad + serial port), and XInput
(`045E:028E`, Xbox 360 interfaces only, so no serial port). DS4 mode reports gyro at 16 per °/s and accel at 8192 per g
and says so in its calibration report, so hosts scale it correctly. The left controller's IMU is mounted rotated 90°
about Z from the right's (left X = right Y, left Y = −right X, Z the same; measured with both at rest in two poses), so
the firmware turns left samples into the right controller's axes before applying the `imu_map` setting. It also removes
each gyro's resting offset (0.3–1.7 °/s per axis; the controllers don't), re-measured whenever a controller lies still
for about a second (`gamepad.rs`, from 1.3.0).

**Front panel.** RGB LED: red P0.08, green P0.12, blue P1.09 (active low). Button: P1.06.

**Updating in place.** The bootloader checks the application against a stored CRC that the application can't
change, so `host/selfupdate.py` sets four spare bytes in the vector table (offset `0x1C`) to make the new image's
CRC match, and the firmware writes the image from a routine running in RAM. A watchdog resets the dongle if it ever
stops responding for 8 s.

## Adding another controller
1. **Advertisement.** Put the controller in pairing mode and run `host/sniff_adv.py`: note its type, ids and
   serial, and whether the packet layout matches the table above.
2. **Device management.** Run `host/dm.py info`. If the version and battery reads work, the pairing commands
   probably match too.
3. **Pair and join.** `host/dm.py pair`, then `host/link.py --raw`: does it join, with what type value?
4. **Map its data.** `host/link.py --changes` while you press, squeeze and move things; compare with
   [Controller data](#controller-data).
5. **Share it.** Open a pull request or an issue with what you found. Partial results help.
