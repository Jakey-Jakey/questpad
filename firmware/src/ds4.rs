//! DS4 compatibility mode: present as a wired DualShock 4 so Steam Input, SDL, macOS and Linux
//! (`hid-playstation`) use it with no setup, including gyro (from one controller's IMU), rumble
//! and battery.
//!
//! Input report 0x01 (64 B) offsets follow Linux `hid-playstation` / SDL's PS4 driver:
//!   1..5 LX LY RX RY u8 (128 = centre), 5 hat | Square Cross Circle Triangle, 6 L1 R1 L2 R2
//!   Share Options L3 R3, 7 PS | Touchpad | counter << 2, 8 L2, 9 R2, 10..12 sensor timestamp
//!   (5.33 µs), 12 temperature, 13..19 gyro xyz, 19..25 accel xyz, 30 battery (level 0..10,
//!   bit 4 = cable), 33.. touch reports (none: "not touching" bit set).
//! Output report 0x05: [4] weak (right) motor, [5] strong (left) motor.
//!
//! IMU units: we report gyro at 16 LSB per deg/s and accel at 8192 LSB per g, and the
//! calibration feature report (0x02) states exactly that, so hosts scale it correctly.

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_usb::class::hid::{ReportId, RequestHandler};
use embassy_usb::control::OutResponse;

use crate::gamepad::{self, Pad};

pub const VID: u16 = 0x054C;
pub const PID: u16 = 0x09CC; // DualShock 4 (2nd gen)
pub const IN_LEN: usize = 64;
pub const OUT_LEN: usize = 64;

/// Wired DualShock 4 HID report descriptor.
///
/// From GP2040-CE `headers/drivers/ps4/PS4Descriptors.h`:
/// SPDX-License-Identifier: MIT
/// Copyright (c) 2024 OpenStickCommunity (gp2040-ce.info)
/// Permission is hereby granted, free of charge, to any person obtaining a copy of this software
/// and associated documentation files (the "Software"), to deal in the Software without
/// restriction, including without limitation the rights to use, copy, modify, merge, publish,
/// distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the
/// Software is furnished to do so, subject to the following conditions: The above copyright
/// notice and this permission notice shall be included in all copies or substantial portions of
/// the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
/// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
/// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
/// LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
/// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
/// DEALINGS IN THE SOFTWARE.
#[rustfmt::skip]
pub const REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01,           // Usage Page (Generic Desktop Ctrls)
    0x09, 0x05,           // Usage (Game Pad)
    0xA1, 0x01,           // Collection (Application)
    0x85, 0x01,           // Report ID (1)
    0x09, 0x30,           // Usage (X)
    0x09, 0x31,           // Usage (Y)
    0x09, 0x32,           // Usage (Z)
    0x09, 0x35,           // Usage (Rz)
    0x15, 0x00,           // Logical Minimum (0)
    0x26, 0xFF, 0x00,     // Logical Maximum (255)
    0x75, 0x08,           // Report Size (8)
    0x95, 0x04,           // Report Count (4)
    0x81, 0x02,           // Input

    0x09, 0x39,           // Usage (Hat switch)
    0x15, 0x00,           // Logical Minimum (0)
    0x25, 0x07,           // Logical Maximum (7)
    0x35, 0x00,           // Physical Minimum (0)
    0x46, 0x3B, 0x01,     // Physical Maximum (315)
    0x65, 0x14,           // Unit (System: English Rotation, Length: Centimeter)
    0x75, 0x04,           // Report Size (4)
    0x95, 0x01,           // Report Count (1)
    0x81, 0x42,           // Input

    0x65, 0x00,           // Unit (None)
    0x05, 0x09,           // Usage Page (Button)
    0x19, 0x01,           // Usage Minimum (0x01)
    0x29, 0x0E,           // Usage Maximum (0x0E)
    0x15, 0x00,           // Logical Minimum (0)
    0x25, 0x01,           // Logical Maximum (1)
    0x75, 0x01,           // Report Size (1)
    0x95, 0x0E,           // Report Count (14)
    0x81, 0x02,           // Input

    0x06, 0x00, 0xFF,     // Usage Page (Vendor Defined 0xFF00)
    0x09, 0x20,           // Usage (0x20)
    0x75, 0x06,           // Report Size (6)
    0x95, 0x01,           // Report Count (1)
    0x81, 0x02,           // Input

    0x05, 0x01,           // Usage Page (Generic Desktop Ctrls)
    0x09, 0x33,           // Usage (Rx)
    0x09, 0x34,           // Usage (Ry)
    0x15, 0x00,           // Logical Minimum (0)
    0x26, 0xFF, 0x00,     // Logical Maximum (255)
    0x75, 0x08,           // Report Size (8)
    0x95, 0x02,           // Report Count (2)
    0x81, 0x02,           // Input

    0x06, 0x00, 0xFF,     // Usage Page (Vendor Defined 0xFF00)
    0x09, 0x21,           // Usage (0x21)
    0x95, 0x36,           // Report Count (54)
    0x81, 0x02,           // Input

    0x85, 0x05,           // Report ID (5)
    0x09, 0x22,           // Usage (0x22)
    0x95, 0x1F,           // Report Count (31)
    0x91, 0x02,           // Output

    0x85, 0x03,           // Report ID (3)
    0x0A, 0x21, 0x27,     // Usage (0x2721)
    0x95, 0x2F,           // Report Count (47)
    0xB1, 0x02,           // Feature

    0x85, 0x02,           // Report ID (2)
    0x09, 0x24,           // Usage (0x24)
    0x95, 0x24,           // Report Count (36)
    0xB1, 0x02,           // Feature
    0x85, 0x08,           // Report ID (8)
    0x09, 0x25,           // Usage (0x25)
    0x95, 0x03,           // Report Count (3)
    0xB1, 0x02,           // Feature
    0x85, 0x10,           // Report ID (16)
    0x09, 0x26,           // Usage (0x26)
    0x95, 0x04,           // Report Count (4)
    0xB1, 0x02,           // Feature
    0x85, 0x11,           // Report ID (17)
    0x09, 0x27,           // Usage (0x27)
    0x95, 0x02,           // Report Count (2)
    0xB1, 0x02,           // Feature
    0x85, 0x12,           // Report ID (18)
    0x06, 0x02, 0xFF,     // Usage Page (Vendor Defined 0xFF02)
    0x09, 0x21,           // Usage (0x21)
    0x95, 0x0F,           // Report Count (15)
    0xB1, 0x02,           // Feature
    0x85, 0x13,           // Report ID (19)
    0x09, 0x22,           // Usage (0x22)
    0x95, 0x16,           // Report Count (22)
    0xB1, 0x02,           // Feature
    0x85, 0x14,           // Report ID (20)
    0x06, 0x05, 0xFF,     // Usage Page (Vendor Defined 0xFF05)
    0x09, 0x20,           // Usage (0x20)
    0x95, 0x10,           // Report Count (16)
    0xB1, 0x02,           // Feature
    0x85, 0x15,           // Report ID (21)
    0x09, 0x21,           // Usage (0x21)
    0x95, 0x2C,           // Report Count (44)
    0xB1, 0x02,           // Feature
    0x06, 0x80, 0xFF,     // Usage Page (Vendor Defined 0xFF80)
    0x85, 0x80,           // Report ID (128)
    0x09, 0x20,           // Usage (0x20)
    0x95, 0x06,           // Report Count (6)
    0xB1, 0x02,           // Feature
    0x85, 0x81,           // Report ID (129)
    0x09, 0x21,           // Usage (0x21)
    0x95, 0x06,           // Report Count (6)
    0xB1, 0x02,           // Feature
    0x85, 0x82,           // Report ID (130)
    0x09, 0x22,           // Usage (0x22)
    0x95, 0x05,           // Report Count (5)
    0xB1, 0x02,           // Feature
    0x85, 0x83,           // Report ID (131)
    0x09, 0x23,           // Usage (0x23)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0x84,           // Report ID (132)
    0x09, 0x24,           // Usage (0x24)
    0x95, 0x04,           // Report Count (4)
    0xB1, 0x02,           // Feature
    0x85, 0x85,           // Report ID (133)
    0x09, 0x25,           // Usage (0x25)
    0x95, 0x06,           // Report Count (6)
    0xB1, 0x02,           // Feature
    0x85, 0x86,           // Report ID (134)
    0x09, 0x26,           // Usage (0x26)
    0x95, 0x06,           // Report Count (6)
    0xB1, 0x02,           // Feature
    0x85, 0x87,           // Report ID (135)
    0x09, 0x27,           // Usage (0x27)
    0x95, 0x23,           // Report Count (35)
    0xB1, 0x02,           // Feature
    0x85, 0x88,           // Report ID (136)
    0x09, 0x28,           // Usage (0x28)
    0x95, 0x22,           // Report Count (34)
    0xB1, 0x02,           // Feature
    0x85, 0x89,           // Report ID (137)
    0x09, 0x29,           // Usage (0x29)
    0x95, 0x02,           // Report Count (2)
    0xB1, 0x02,           // Feature
    0x85, 0x90,           // Report ID (144)
    0x09, 0x30,           // Usage (0x30)
    0x95, 0x05,           // Report Count (5)
    0xB1, 0x02,           // Feature
    0x85, 0x91,           // Report ID (145)
    0x09, 0x31,           // Usage (0x31)
    0x95, 0x03,           // Report Count (3)
    0xB1, 0x02,           // Feature
    0x85, 0x92,           // Report ID (146)
    0x09, 0x32,           // Usage (0x32)
    0x95, 0x03,           // Report Count (3)
    0xB1, 0x02,           // Feature
    0x85, 0x93,           // Report ID (147)
    0x09, 0x33,           // Usage (0x33)
    0x95, 0x0C,           // Report Count (12)
    0xB1, 0x02,           // Feature
    0x85, 0xA0,           // Report ID (160)
    0x09, 0x40,           // Usage (0x40)
    0x95, 0x06,           // Report Count (6)
    0xB1, 0x02,           // Feature
    0x85, 0xA1,           // Report ID (161)
    0x09, 0x41,           // Usage (0x41)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xA2,           // Report ID (162)
    0x09, 0x42,           // Usage (0x42)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xA3,           // Report ID (163)
    0x09, 0x43,           // Usage (0x43)
    0x95, 0x30,           // Report Count (48)
    0xB1, 0x02,           // Feature
    0x85, 0xA4,           // Report ID (164)
    0x09, 0x44,           // Usage (0x44)
    0x95, 0x0D,           // Report Count (13)
    0xB1, 0x02,           // Feature
    0x85, 0xA5,           // Report ID (165)
    0x09, 0x45,           // Usage (0x45)
    0x95, 0x15,           // Report Count (21)
    0xB1, 0x02,           // Feature
    0x85, 0xA6,           // Report ID (166)
    0x09, 0x46,           // Usage (0x46)
    0x95, 0x15,           // Report Count (21)
    0xB1, 0x02,           // Feature
    0x85, 0xA7,           // Report ID (247)
    0x09, 0x4A,           // Usage (0x4A)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xA8,           // Report ID (250)
    0x09, 0x4B,           // Usage (0x4B)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xA9,           // Report ID (251)
    0x09, 0x4C,           // Usage (0x4C)
    0x95, 0x08,           // Report Count (8)
    0xB1, 0x02,           // Feature
    0x85, 0xAA,           // Report ID (252)
    0x09, 0x4E,           // Usage (0x4E)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xAB,           // Report ID (253)
    0x09, 0x4F,           // Usage (0x4F)
    0x95, 0x39,           // Report Count (57)
    0xB1, 0x02,           // Feature
    0x85, 0xAC,           // Report ID (254)
    0x09, 0x50,           // Usage (0x50)
    0x95, 0x39,           // Report Count (57)
    0xB1, 0x02,           // Feature
    0x85, 0xAD,           // Report ID (255)
    0x09, 0x51,           // Usage (0x51)
    0x95, 0x0B,           // Report Count (11)
    0xB1, 0x02,           // Feature
    0x85, 0xAE,           // Report ID (256)
    0x09, 0x52,           // Usage (0x52)
    0x95, 0x01,           // Report Count (1)
    0xB1, 0x02,           // Feature
    0x85, 0xAF,           // Report ID (175)
    0x09, 0x53,           // Usage (0x53)
    0x95, 0x02,           // Report Count (2)
    0xB1, 0x02,           // Feature
    0x85, 0xB0,           // Report ID (176)
    0x09, 0x54,           // Usage (0x54)
    0x95, 0x3F,           // Report Count (63)
    0xB1, 0x02,           // Feature
    0xC0,                 // End Collection

    0x06, 0xF0, 0xFF,     // Usage Page (Vendor Defined 0xFFF0)
    0x09, 0x40,           // Usage (0x40)
    0xA1, 0x01,           // Collection (Application)
    0x85, 0xF0,           // Report ID (-16) AUTH F0
    0x09, 0x47,           // Usage (0x47)
    0x95, 0x3F,           // Report Count (63)
    0xB1, 0x02,           // Feature
    0x85, 0xF1,           // Report ID (-15) AUTH F1
    0x09, 0x48,           // Usage (0x48)
    0x95, 0x3F,           // Report Count (63)
    0xB1, 0x02,           // Feature
    0x85, 0xF2,           // Report ID (-14) AUTH F2
    0x09, 0x49,           // Usage (0x49)
    0x95, 0x0F,           // Report Count (15)
    0xB1, 0x02,           // Feature
    0x85, 0xF3,           // Report ID (-13) Auth F3 (Reset)
    0x0A, 0x01, 0x47,     // Usage (0x4701)
    0x95, 0x07,           // Report Count (7)
    0xB1, 0x02,           // Feature
    0xC0,                 // End Collection
];

/// Feature reports the descriptor declares: (id, length including the id byte).
const FEATURES: &[(u8, usize)] = &[
    (0x02, 37), (0x03, 48), (0x08, 4), (0x10, 5), (0x11, 3), (0x12, 16), (0x13, 23), (0x14, 17),
    (0x15, 45), (0x80, 7), (0x81, 7), (0x82, 6), (0x83, 2), (0x84, 5), (0x85, 7), (0x86, 7),
    (0x87, 36), (0x88, 35), (0x89, 3), (0x90, 6), (0x91, 4), (0x92, 4), (0x93, 13), (0xA0, 7),
    (0xA1, 2), (0xA2, 2), (0xA3, 49), (0xA4, 14), (0xA5, 22), (0xA6, 22), (0xA7, 2), (0xA8, 2),
    (0xA9, 9), (0xAA, 2), (0xAB, 58), (0xAC, 58), (0xAD, 12), (0xAE, 2), (0xAF, 3), (0xB0, 64),
];

static COUNTER: AtomicU8 = AtomicU8::new(0);

fn stick(v: i16) -> u8 {
    ((v >> 8) + 128) as u8
}

/// Controller IMU -> DS4 frame. The Touch IMU axes map onto the DS4's (x right, y up, z toward
/// the player; gyro = rotation about those axes); see PROTOCOL.md (IMU axes).
fn imu_axes(p: &Pad) -> ([i16; 3], [i16; 3]) {
    let g = p.imu.gyro;
    let a = p.imu.accel;
    let sat = |v: i32| v.clamp(-32768, 32767) as i16;
    // Touch: ~8.19 LSB/(deg/s) -> 16; ~1024 LSB/g -> 8192.
    let gyro = [sat(g[0] as i32 * 2), sat(g[1] as i32 * 2), sat(g[2] as i32 * 2)];
    let accel = [sat(a[0] as i32 * 8), sat(a[1] as i32 * 8), sat(a[2] as i32 * 8)];
    let m = crate::settings::get().imu_map;
    let pick = |v: &[i16; 3], i: usize| {
        let axis = (m[i] & 0x03) as usize;
        let x = v[axis.min(2)];
        if m[i] & 0x80 != 0 { x.saturating_neg() } else { x }
    };
    ([pick(&gyro, 0), pick(&gyro, 1), pick(&gyro, 2)], [pick(&accel, 0), pick(&accel, 1), pick(&accel, 2)])
}

pub fn report(p: &Pad) -> [u8; IN_LEN] {
    use gamepad::*;
    let mut r = [0u8; IN_LEN];
    r[0] = 0x01;
    r[1] = stick(p.lx);
    r[2] = stick(p.ly);
    r[3] = stick(p.rx);
    r[4] = stick(p.ry);
    let b = p.buttons;
    let on = |m: u16, v: u8| if b & m != 0 { v } else { 0 };
    r[5] = 0x08 | on(BTN_X, 0x10) | on(BTN_A, 0x20) | on(BTN_B, 0x40) | on(BTN_Y, 0x80);
    r[6] = on(BTN_LB, 0x01)
        | on(BTN_RB, 0x02)
        | if p.lt > 20 { 0x04 } else { 0 }
        | if p.rt > 20 { 0x08 } else { 0 }
        | on(BTN_BACK, 0x10)
        | on(BTN_START, 0x20)
        | on(BTN_LS, 0x40)
        | on(BTN_RS, 0x80);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed) & 0x3F;
    r[7] = on(BTN_GUIDE, 0x01) | counter << 2;
    r[8] = p.lt;
    r[9] = p.rt;
    // Sensor timestamp in 16/3 µs units.
    let ts = (p.imu.ts as u64 * 3 / 16) as u16;
    r[10..12].copy_from_slice(&ts.to_le_bytes());
    r[12] = 0;
    let (gyro, accel) = imu_axes(p);
    for i in 0..3 {
        r[13 + 2 * i..15 + 2 * i].copy_from_slice(&gyro[i].to_le_bytes());
        r[19 + 2 * i..21 + 2 * i].copy_from_slice(&accel[i].to_le_bytes());
    }
    // Battery: level 0..10, not on a cable (the controllers run on AA cells).
    r[30] = p.battery.map_or(0, |b| (b / 10).min(10));
    // Touchpad: one (empty) touch report with both fingers lifted.
    r[33] = 1;
    r[35] = 0x80;
    r[39] = 0x80;
    r
}

/// Calibration (feature 0x02, USB layout: gyro plus / minus interleaved per axis): biases 0,
/// gyro ±32000 = ±2000 deg/s (16 LSB per deg/s), accel ±8192 = ±1 g.
fn calibration(buf: &mut [u8]) {
    let v: [i16; 17] = [
        0, 0, 0, // pitch, yaw, roll bias
        32000, -32000, 32000, -32000, 32000, -32000, // pitch +/-, yaw +/-, roll +/-
        2000, 2000, // gyro speed +/-
        8192, -8192, 8192, -8192, 8192, -8192, // accel x, y, z +/-
    ];
    for (i, x) in v.iter().enumerate() {
        buf[1 + 2 * i..3 + 2 * i].copy_from_slice(&x.to_le_bytes());
    }
}

/// A stable fake MAC address from the dongle's FICR device id.
fn mac() -> [u8; 6] {
    let id0 = unsafe { core::ptr::read_volatile(0x1000_0060 as *const u32) }.to_le_bytes();
    let id1 = unsafe { core::ptr::read_volatile(0x1000_0064 as *const u32) }.to_le_bytes();
    [id0[0], id0[1], id0[2], id0[3], id1[0], 0x02]
}

pub struct Handler;

impl RequestHandler for Handler {
    fn get_report(&mut self, id: ReportId, buf: &mut [u8]) -> Option<usize> {
        let ReportId::Feature(id) = id else { return None };
        let &(_, len) = FEATURES.iter().find(|(i, _)| *i == id)?;
        if buf.len() < len {
            return None;
        }
        buf[..len].fill(0);
        buf[0] = id;
        match id {
            0x02 => calibration(buf),
            // Pairing info: [client MAC][08 25 00][host MAC].
            0x12 => {
                buf[1..7].copy_from_slice(&mac());
                buf[7..10].copy_from_slice(&[0x08, 0x25, 0x00]);
            }
            0x81 => buf[1..7].copy_from_slice(&mac()),
            // Firmware info: build date / time strings, then hw (35) and fw (41) versions.
            0xA3 => {
                buf[1..12].copy_from_slice(b"Oct 10 2026");
                buf[17..25].copy_from_slice(b"00:00:00");
                buf[35] = 0x00;
                buf[36] = 0x01;
                buf[41] = 0x00;
                buf[42] = 0x01;
            }
            _ => {}
        }
        Some(len)
    }

    fn set_report(&mut self, id: ReportId, data: &[u8]) -> OutResponse {
        // Output report 0x05 (with or without the id byte in front).
        let d = match (id, data.first()) {
            (_, Some(0x05)) => &data[1..],
            (ReportId::Out(5), _) => data,
            _ => return OutResponse::Accepted,
        };
        if d.len() >= 5 && d[0] & 0x01 != 0 {
            // [flags][0][0][weak = right][strong = left]
            crate::link::rumble(d[4], d[3]);
        }
        OutResponse::Accepted
    }
}
