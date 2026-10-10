# How QuestPad works

A summary of the Quest 1 Touch controllers' radio protocol as QuestPad implements it, and of the dongle firmware.
Everything here was verified on hardware (controller firmware app 1.17.2 / bootloader 1.13.0). Byte orders are
little-endian unless noted. Meta's internal name for the protocol is **Pulsar**.

## Radio (PHY)
- nRF "2 Mbit" mode (`MODE=1`), fast ramp-up, no whitening.
- 5-byte addresses (4-byte base + 1-byte prefix), big-endian on air (`PCNF1 = 0x01040000 | maxlen`).
- 24-bit CRC over the address too: `CRCCNF=3`, poly `0x1108421` (with the implicit top bit), init `0xFFFFFF`.
- Packet layout in RAM: `[S0][LEN][S1][payload]` (`PCNF0 = 0x00100108`) on the main link;
  `[LEN][payload]` (`PCNF0 = 8`) in pairing mode.

## Pairing (device-management mode)
Holding **Oculus + Y** (left) or **Oculus + B** (right) starts the controller's bootloader in a pairing mode
("DM mode"):
- It advertises every 102.2 ms for about 60 s on 2402 MHz, base address `0xFACEB00C`, prefix `0xAA`.
  Advertisement: `[2][type u32][id0 u32][id1 u32][4 B][serial, 14 ASCII][status]`; type `0x13001101` = left,
  `0x13011101` = right (bit 16 = hand).
- A host answers on 2426 MHz, base address = `id0`, prefix `0xAA`, starting 1000 µs after the end of an
  advertisement. The two sides then alternate, each transmitting 1000 µs after the end of the other's packet.
- Host packet: `[LEN][hdr][seq][payload]`, `hdr = command << 1 | read`. Replies echo `seq`; bit 7 of the reply
  header = error. Empty polls `[2][0][seq]` keep the link alive.
- Useful commands: `0x03` read serial, `0x24` / `0x25` read app / bootloader version, `0x2F` read battery,
  `0x14` **write pairing record** (20 bytes: `network_id u32`, `link_key[16]`), `0x15` reset into the main app.
  The Quest 1 controllers take the record in plaintext; newer controllers appear to use a key-exchange pairing.

QuestPad generates a random network id (radio-address friendly: many bit transitions, no long runs) and a random
128-bit key on the dongle, writes them to each controller, and stores them in the dongle's flash.

## Main link
The dongle plays the headset's role: a **2000 µs frame**, one beacon per frame from the host, then up to five
device time slots.

**Addressing.** Base address = `network_id`. The host's beacon uses prefix `0xF0`; a device in slot *s* transmits
with prefix *s* + 1, so the host listens on logical addresses 1..5 and `RXMATCH − 1` is the slot.

**Channels.** 37 channels (the BLE data-channel set): index 0..36 → `FREQUENCY` 4, 6, …, 24, 28, …, 78
(2404–2478 MHz, skipping 2426). Hopping is BLE channel-selection algorithm #1: `unmapped = (cur + hop) % 37`,
remapped through the used-channel list if masked, with `hop = (network_id & 0xFF) % 11 + 5`. A controller
looking for the host listens on channel indices 0, 17 and 36 in turn until it hears a beacon.

**Beacon** (`S0 = 4`, plaintext). Payload:

| bytes | content |
|---|---|
| 0..5 | 3 flag bits, then the 37-bit channel map `<< 3` |
| 5 | current (unmapped) channel index |
| 6..8 | session id (any value; part of the IV) |
| 8..14 | 48-bit timestamp in µs (part of the IV; must advance by exactly 2000 per frame) |
| 14.. | control data (below) |

**Slots.** Devices transmit at 350, 575, 875, 1175 and 1475 µs after the beacon for slots 0..4. Slot 0 is shared,
for joining. A controller declares the link lost after 24 missed beacons.

**Joining.** A controller sends, in slot 0: `[0][1][device_id 8][type u32]`. The host answers in the next
beacon's control data with `[1][ack mask][2][device_id 8][slot 1..4][0][1]` (padded to 36 bytes); `3` instead
of `2` rejects. Otherwise control data is `[downlink mask][ack mask]` (bit *s* of the ack mask = "got your slot-*s*
packet last frame", sent on a good CRC).

**Encryption.** Uplinks are AES-CCM (nRF CCM peripheral, BLE-style, 4-byte MIC, extended-length 2 Mbit mode)
with the link key, packet counter 0, direction 0, and IV = beacon timestamp (u48) + session (u16). The nRF CCM
decrypts at roughly radio speed (~310 µs for a full packet), so the firmware queues uplinks and finishes them in
the CCM interrupt.

## Uplink data
Decrypted uplink: `[slot][1][0x40 | seq][log item, 4 B][notification entries…]`. The 4-byte log item carries the
controller's own text debug log, a few characters at a time (`host/link.py --log` shows it).

Notification entries: `[id:5 | len_lo:3 << 5][len_hi:4 | seq:3 << 4 | last << 7][data]`.

| id | content |
|---|---|
| 0 | battery % |
| 1 | IMU, 18 B: timestamp u48 (µs), accel xyz i16 (~1024 / g), gyro xyz i16 (~8.2 / °/s); every 2 ms |
| 2 | thumbstick x, y i16 (+x right, +y up; 0, 0 at rest, already calibrated) |
| 3 | trigger, grip: two 12-bit values in 3 bytes, inverted (4095 = released, 0 = fully squeezed) |
| 4 | buttons: bit 0 X/A, bit 1 Y/B, bit 2 stick click, bit 3 Menu/Oculus (bit 4 always set) |
| 5 | capacitive touch bits (0x01 X/A, 0x02 Y/B, 0x04 stick, …) |
| 6, 7 | unknown |

IMU axes (right controller, held naturally): −Z points up and −Y points forward.

## Downlink (commands to a controller)
A beacon can carry one request: control data `[1 << slot][ack mask][register][seq | read << 4][data ≤ 32 B]`.
A controller takes a request when its seq (or register, or read bit) differs from the last one it took, and
echoes the seq of the last request frame it saw in the low nibble of its uplink flags. That echo comes from any
beacon, whoever it was addressed to, so QuestPad uses one seq counter for all controllers and resends a request
until the target controller echoes it. Read replies come back as uplink frames without the `0x40` bit:
`[slot][register][flags][data]` (flags bit 5 = error).

Registers worth knowing (`host/regs.py` reads or writes them):

| register | what |
|---|---|
| `0x01` read | maker / product / hand / hardware revision strings |
| `0x02`, `0x03` read | board serial, device serial |
| `0x24`, `0x25` read | app / bootloader version |
| `0x2F` read | battery: f32 %, then u16 cell voltage in mV |
| `0x31`, `0x32` read | IMU part and configuration |
| `0x05` write | **power off** (wakes on motion or a button) |
| `0x06` write | 1 = standby (uplinks almost stop), 0 = active |
| `0x97` write | **vibration strength**, 1 byte (stays on until set to 0) |
| `0x28` read / write | IR LED sync: period (33333 or 40000 µs), pulse width (1–99 µs) |
| `0x13` write | clears the controller's pairing — avoid |
| `0x1c` write | radio test mode until reset — avoid |

## Dongle firmware (`firmware/`)
Rust + [embassy](https://embassy.dev) on the nRF52840 Dongle (PCA10059), application at `0x1000` under Nordic's
Open USB DFU bootloader.

- **Flash layout:** settings at `0xDE000`, pairing record at `0xDF000`; the firmware image ends below them.
- **USB modes:** HID gamepad + serial port (`1209:0001`), DS4 (`054C:09CC`, HID + serial port) and XInput
  (`045E:028E`, vendor interfaces only). The DS4 descriptor and the XInput layout follow GP2040-CE. DS4 mode
  reports gyro at 16 LSB per °/s and accel at 8192 LSB per g and says so in its calibration feature report.
- **Front panel:** RGB LED (red P0.08, green P0.12, blue P1.09, active low) and the button on P1.06.
- **Updating in place:** the bootloader checks the application's CRC against a stored value, so `host/selfupdate.py`
  sets four spare bytes in the vector table (offset `0x1C`) to make the new image match it; the firmware then
  writes the image from a RAM routine. A watchdog (8 s) resets the dongle if it ever hangs.
- **Serial protocol** (used by the control panel and `host/` tools): COBS-framed commands and events, documented
  at the top of `firmware/src/main.rs`.

## Adding another controller
1. Put it in pairing mode and look at its advertisement (`host/sniff_adv.py`): type, ids, serial.
2. Try `host/dm.py info` to see whether the device-management commands match.
3. Pair it (`host/dm.py pair`) and watch it join (`host/link.py --raw`); check its type value and slot.
4. Map its notifications with `host/link.py --changes` while pressing and moving things.
5. Open a pull request with what you found, even if it only partly works.
