//! USB HID gamepad with Xbox-360-style controls (one combined pad for both Touch controllers).
//!
//! Report (13 bytes):
//!   [0..2]  buttons u16: A B X Y LB RB Back Start LS RS Guide (bits 0..10)
//!   [2]     hat (low nibble, 0..7 clockwise from up, 8 = released)
//!   [3]     left trigger u8, [4] right trigger u8
//!   [5..13] left X, left Y, right X, right Y as i16 LE (Y down is positive, like HID)
//! Output report (2 bytes): rumble amplitude for the left, right controller (0 = off).

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;

pub const REPORT_LEN: usize = 13;
pub const OUT_REPORT_LEN: usize = 2;

#[rustfmt::skip]
pub const REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x05,       // Usage (Gamepad)
    0xA1, 0x01,       // Collection (Application)
    0x05, 0x09,       //   Usage Page (Button)
    0x19, 0x01,       //   Usage Minimum (1)
    0x29, 0x10,       //   Usage Maximum (16)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x01,       //   Logical Maximum (1)
    0x75, 0x01,       //   Report Size (1)
    0x95, 0x10,       //   Report Count (16)
    0x81, 0x02,       //   Input (Data, Var, Abs)
    0x05, 0x01,       //   Usage Page (Generic Desktop)
    0x09, 0x39,       //   Usage (Hat Switch)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x07,       //   Logical Maximum (7)
    0x35, 0x00,       //   Physical Minimum (0)
    0x46, 0x3B, 0x01, //   Physical Maximum (315)
    0x65, 0x14,       //   Unit (Degrees)
    0x75, 0x04,       //   Report Size (4)
    0x95, 0x01,       //   Report Count (1)
    0x81, 0x42,       //   Input (Data, Var, Abs, Null State)
    0x65, 0x00,       //   Unit (None)
    0x75, 0x04,       //   Report Size (4)
    0x95, 0x01,       //   Report Count (1)
    0x81, 0x03,       //   Input (Const) padding
    0x09, 0x32,       //   Usage (Z)  left trigger
    0x09, 0x35,       //   Usage (Rz) right trigger
    0x15, 0x00,       //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08,       //   Report Size (8)
    0x95, 0x02,       //   Report Count (2)
    0x81, 0x02,       //   Input (Data, Var, Abs)
    0x09, 0x30,       //   Usage (X)  left stick
    0x09, 0x31,       //   Usage (Y)
    0x09, 0x33,       //   Usage (Rx) right stick
    0x09, 0x34,       //   Usage (Ry)
    0x16, 0x00, 0x80, //   Logical Minimum (-32768)
    0x26, 0xFF, 0x7F, //   Logical Maximum (32767)
    0x75, 0x10,       //   Report Size (16)
    0x95, 0x04,       //   Report Count (4)
    0x81, 0x02,       //   Input (Data, Var, Abs)
    0x06, 0x00, 0xFF, //   Usage Page (Vendor 0xFF00)
    0x09, 0x01,       //   Usage (rumble: left, right amplitude)
    0x15, 0x00,       //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08,       //   Report Size (8)
    0x95, 0x02,       //   Report Count (2)
    0x91, 0x02,       //   Output (Data, Var, Abs)
    0xC0,             // End Collection
];

/// Neutral report: nothing pressed, hat released, sticks centred.
pub const NEUTRAL: [u8; REPORT_LEN] = [0, 0, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Latest report to send; set from the radio link (or the serial bridge).
pub static REPORT: Signal<CriticalSectionRawMutex, [u8; REPORT_LEN]> = Signal::new();

/// Host-provided report (bridge CMD_PAD_SET); disables the link feed until the link restarts.
pub fn set(report: &[u8]) -> bool {
    if report.len() != REPORT_LEN {
        return false;
    }
    AUTO.store(false, core::sync::atomic::Ordering::Relaxed);
    let mut r = [0u8; REPORT_LEN];
    r.copy_from_slice(report);
    REPORT.signal(r);
    true
}

// ---- Touch controllers -> report, fed straight from the radio link ----
//
// Notification ids (verified on air, see NOTES.md): 2 stick x,y i16 (+y up); 3 trigger, grip
// 12-bit packed, inverted (4095 released); 4 buttons (bit0 A/X, bit1 B/Y, bit2 stick click,
// bit3 Oculus/menu); 5 capacitive touch bits.

use core::sync::atomic::{AtomicBool, Ordering};

/// Whether the link drives the report; a host CMD_PAD_SET takes over until the link restarts.
pub static AUTO: AtomicBool = AtomicBool::new(true);

const BTN_A: u16 = 1 << 0;
const BTN_B: u16 = 1 << 1;
const BTN_X: u16 = 1 << 2;
const BTN_Y: u16 = 1 << 3;
const BTN_LB: u16 = 1 << 4;
const BTN_RB: u16 = 1 << 5;
const BTN_START: u16 = 1 << 7;
const BTN_LS: u16 = 1 << 8;
const BTN_RS: u16 = 1 << 9;
const BTN_GUIDE: u16 = 1 << 10;

const ANALOG_MAX: u16 = 4095;
const STICK_FULL: i32 = 32000;
const DEADZONE: i32 = STICK_FULL * 8 / 100;

#[derive(Clone, Copy, PartialEq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct Hand {
    side: Option<Side>,
    buttons: u8,
    touch: u8,
    stick: (i16, i16),
    trigger: u16,
    grip: u16,
}

const IDLE: Hand = Hand { side: None, buttons: 0, touch: 0, stick: (0, 0), trigger: 0, grip: 0 };

/// Per link slot (only touched from the radio-side interrupts, which share one priority).
static mut HANDS: [Hand; 5] = [IDLE; 5];

fn hands() -> &'static mut [Hand; 5] {
    unsafe { &mut *core::ptr::addr_of_mut!(HANDS) }
}

/// Controller type from its join request: bit 16 set = right hand (0x13001101 / 0x13011101).
pub fn link_join(slot: u8, dev_type: u32) {
    let side = if dev_type & 0x1_0000 != 0 { Side::Right } else { Side::Left };
    hands()[slot as usize] = Hand { side: Some(side), ..IDLE };
    publish();
}

pub fn link_drop(slot: u8) {
    hands()[slot as usize] = IDLE;
    publish();
}

/// One notification payload (the entries after the 4-byte log item).
pub fn link_notifications(slot: u8, mut buf: &[u8]) {
    let slot = slot as usize;
    let h = &mut hands()[slot];
    let mut changed = false;
    while buf.len() >= 2 {
        let id = buf[0] & 0x1F;
        let len = ((buf[0] >> 5) | ((buf[1] & 0x0F) << 3)) as usize;
        if id == 0 && len == 0 || buf.len() < 2 + len {
            break;
        }
        let d = &buf[2..2 + len];
        match id {
            4 if len >= 1 => h.buttons = d[0],
            1 if len >= 18 => {
                // IMU sample: [ts u48][accel xyz i16][gyro xyz i16] -> stillness for the power policy
                let g = |i: usize| (i16::from_le_bytes([d[i], d[i + 1]]) as i32).abs();
                if g(12).max(g(14)).max(g(16)) > STILL_GYRO {
                    unsafe { (*core::ptr::addr_of_mut!(ACTIVITY))[slot].moving = true };
                }
                buf = &buf[2 + len..];
                continue;
            }
            5 if len >= 1 => {
                h.touch = d[0]; // capacitive touch: only feeds the idle timer
                buf = &buf[2 + len..];
                continue;
            }
            2 if len >= 4 => h.stick = (i16::from_le_bytes([d[0], d[1]]), i16::from_le_bytes([d[2], d[3]])),
            3 if len >= 3 => {
                let t = d[0] as u16 | ((d[1] as u16 & 0x0F) << 8);
                let g = (d[1] as u16 >> 4) | ((d[2] as u16) << 4);
                h.trigger = ANALOG_MAX - t.min(ANALOG_MAX);
                h.grip = ANALOG_MAX - g.min(ANALOG_MAX);
            }
            _ => {
                buf = &buf[2 + len..];
                continue;
            }
        }
        changed = true;
        buf = &buf[2 + len..];
    }
    if changed {
        publish();
    }
}

fn axis(v: i16, invert: bool) -> i16 {
    let v = v as i32;
    if v.abs() < DEADZONE {
        return 0;
    }
    let v = (v * 32767 / STICK_FULL).clamp(-32767, 32767);
    (if invert { -v } else { v }) as i16
}

fn trigger(raw: u16) -> u8 {
    (raw as u32 * 255 / ANALOG_MAX as u32) as u8
}

/// Combine all hands into the Xbox-style report (left = left half, right = right half).
fn build() -> [u8; REPORT_LEN] {
    let mut btn = 0u16;
    let (mut lt, mut rt) = (0u8, 0u8);
    let mut sticks = [0i16; 4];
    for h in hands().iter() {
        let b = h.buttons;
        let (face0, face1, click, menu, bumper) = match h.side {
            Some(Side::Left) => (BTN_X, BTN_Y, BTN_LS, BTN_START, BTN_LB),
            Some(Side::Right) => (BTN_A, BTN_B, BTN_RS, BTN_GUIDE, BTN_RB),
            None => continue,
        };
        for (bit, out) in [(0x01, face0), (0x02, face1), (0x04, click), (0x08, menu)] {
            if b & bit != 0 {
                btn |= out;
            }
        }
        if h.grip > ANALOG_MAX / 2 {
            btn |= bumper;
        }
        let (x, y) = (axis(h.stick.0, false), axis(h.stick.1, true)); // HID: +y is down
        if h.side == Some(Side::Left) {
            lt = trigger(h.trigger);
            sticks[0] = x;
            sticks[1] = y;
        } else {
            rt = trigger(h.trigger);
            sticks[2] = x;
            sticks[3] = y;
        }
    }
    let mut r = NEUTRAL;
    r[0..2].copy_from_slice(&btn.to_le_bytes());
    r[3] = lt;
    r[4] = rt;
    for (i, s) in sticks.iter().enumerate() {
        r[5 + 2 * i..7 + 2 * i].copy_from_slice(&s.to_le_bytes());
    }
    r
}

fn publish() {
    if AUTO.load(Ordering::Relaxed) {
        REPORT.signal(build());
    }
}

/// Rumble output reports (interrupt OUT or SET_REPORT) -> controller haptics.
pub struct Rumble;

impl embassy_usb::class::hid::RequestHandler for Rumble {
    fn set_report(&mut self, _id: embassy_usb::class::hid::ReportId, data: &[u8]) -> embassy_usb::control::OutResponse {
        if data.len() < OUT_REPORT_LEN {
            return embassy_usb::control::OutResponse::Rejected;
        }
        crate::link::rumble(data[0], data[1]);
        embassy_usb::control::OutResponse::Accepted
    }
}

// ---- power policy (Quest 1 controllers have no off switch) ----

/// Frames (2 ms) the Oculus / menu button must be held to power off all controllers.
const OFF_HOLD_FRAMES: u32 = 3 * 500;
/// Frames without input before a controller is powered off (10 min).
const IDLE_FRAMES: u32 = 10 * 60 * 500;

/// Gyro magnitude (per axis, ~0.122 dps/LSB) below which a controller counts as at rest (~5 dps).
const STILL_GYRO: i32 = 40;
/// Frames a controller must be at rest (buttons released) before an armed power-off fires,
/// so the motion of putting it down doesn't wake it straight back up.
const STILL_FRAMES: u32 = 500;
/// An armed power-off fires anyway after this long (30 s).
const ARMED_MAX_FRAMES: u32 = 30 * 500;

#[derive(Clone, Copy)]
struct Activity {
    last_buttons: u8,
    hold_since: Option<u32>,
    last_active: u32,
    /// Set by any IMU sample above STILL_GYRO since the last policy check.
    moving: bool,
    still_since: u32,
    armed_at: Option<u32>,
}

const NO_ACTIVITY: Activity =
    Activity { last_buttons: 0, hold_since: None, last_active: 0, moving: false, still_since: 0, armed_at: None };
static mut ACTIVITY: [Activity; 5] = [NO_ACTIVITY; 5];

#[derive(PartialEq)]
pub enum PowerAction {
    None,
    /// Power this controller off now.
    OffThis,
    /// Off gesture held: arm every controller (and buzz to confirm).
    ArmAll,
}

/// Arm a power-off for `slot`: it fires once the controller is put down and still.
pub fn arm_power_off(slot: u8, frame: u32) {
    let a = unsafe { &mut (*core::ptr::addr_of_mut!(ACTIVITY))[slot as usize] };
    a.armed_at = Some(frame);
    a.still_since = frame;
}

fn active(h: &Hand) -> bool {
    h.buttons & 0x0F != 0
        || h.touch & 0x0F != 0 // thumb / finger resting on X Y stick trigger
        || (h.stick.0 as i32).abs() > DEADZONE
        || (h.stick.1 as i32).abs() > DEADZONE
        || h.trigger > ANALOG_MAX / 10
        || h.grip > ANALOG_MAX / 10
}

/// Called after each notification batch from `slot` at link frame `frame`.
pub fn power_policy(slot: u8, frame: u32) -> PowerAction {
    let h = hands()[slot as usize];
    let a = unsafe { &mut (*core::ptr::addr_of_mut!(ACTIVITY))[slot as usize] };
    if h.buttons != a.last_buttons || active(&h) {
        a.last_active = frame;
    }
    a.last_buttons = h.buttons;
    if a.moving || h.buttons & 0x0F != 0 {
        a.still_since = frame;
    }
    a.moving = false;
    if let Some(armed) = a.armed_at {
        let still = frame.wrapping_sub(a.still_since) >= STILL_FRAMES;
        if still || frame.wrapping_sub(armed) >= ARMED_MAX_FRAMES {
            a.armed_at = None;
            return PowerAction::OffThis;
        }
        return PowerAction::None;
    }
    if h.buttons & 0x08 != 0 {
        let since = *a.hold_since.get_or_insert(frame);
        if frame.wrapping_sub(since) >= OFF_HOLD_FRAMES {
            a.hold_since = None;
            return PowerAction::ArmAll;
        }
    } else {
        a.hold_since = None;
    }
    if frame.wrapping_sub(a.last_active) >= IDLE_FRAMES {
        a.last_active = frame;
        return PowerAction::OffThis;
    }
    PowerAction::None
}

/// Reset activity tracking for a newly joined controller.
pub fn power_reset(slot: u8, frame: u32) {
    unsafe {
        (*core::ptr::addr_of_mut!(ACTIVITY))[slot as usize] = Activity { last_active: frame, still_since: frame, ..NO_ACTIVITY };
    }
}
