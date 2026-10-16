//! Touch controllers -> one combined pad state, encoded per USB mode (HID here, DS4 in `ds4.rs`,
//! XInput in `xinput.rs`), plus the power policy (the controllers have no off switch) and the
//! mode gesture.
//!
//! Notification ids (verified on air, see PROTOCOL.md): 0 battery %; 1 IMU [ts u48][accel xyz i16
//! ~1024/g][gyro xyz i16 ~8.2/dps]; 2 stick x,y i16 (+y up, 0,0 at rest: the controllers apply
//! their own calibration); 3 trigger, grip 12-bit packed, inverted (4095 released); 4 buttons
//! (bit0 A/X, bit1 B/Y, bit2 stick click, bit3 Oculus/menu); 5 capacitive touch (bits 0..3 on A/X,
//! B/Y, stick, trigger; bits 4..7 near the same; the thumb rest has no sensor of its own); 6 status
//! flags (bit0 dead battery, bit4 standby); 7 raw touch levels of the same four sensors, u8 each.

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Instant;

use crate::settings;

// ---- combined pad state ----

pub const BTN_A: u16 = 1 << 0;
pub const BTN_B: u16 = 1 << 1;
pub const BTN_X: u16 = 1 << 2;
pub const BTN_Y: u16 = 1 << 3;
pub const BTN_LB: u16 = 1 << 4;
pub const BTN_RB: u16 = 1 << 5;
pub const BTN_BACK: u16 = 1 << 6;
pub const BTN_START: u16 = 1 << 7;
pub const BTN_LS: u16 = 1 << 8;
pub const BTN_RS: u16 = 1 << 9;
pub const BTN_GUIDE: u16 = 1 << 10;

#[derive(Clone, Copy, Default)]
pub struct Imu {
    /// Controller IMU units: accel ~1024 LSB/g, gyro ~8.2 LSB/(deg/s).
    pub accel: [i16; 3],
    pub gyro: [i16; 3],
    /// Controller timestamp, µs (low 32 bits).
    pub ts: u32,
}

/// Everything a USB encoder needs. Sticks use HID convention (+y = down).
#[derive(Clone, Copy, Default)]
pub struct Pad {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    /// Lowest battery % among connected controllers (None if none connected).
    pub battery: Option<u8>,
    pub imu: Imu,
    pub connected: u8,
    /// Thumb down on the sensors chosen in settings (`touch_left` / `touch_right`): bit 0 left, bit 1 right.
    pub thumbs: u8,
}

/// Latest pad state for the active USB mode's encoder.
pub static PAD: Signal<CriticalSectionRawMutex, Pad> = Signal::new();

/// Whether the link drives the gamepad; a host CMD_PAD_SET takes over (HID mode) until the link restarts.
pub static AUTO: AtomicBool = AtomicBool::new(true);

// ---- HID mode report ----
//
// Input report (14 bytes):
//   [0..2]  buttons u16: A B X Y LB RB Back Start LS RS Guide (bits 0..10)
//   [2]     hat (low nibble, 0..7 clockwise from up, 8 = released)
//   [3]     left trigger u8, [4] right trigger u8
//   [5..13] left X, left Y, right X, right Y as i16 LE (Y down is positive, like HID)
//   [13]    battery % (lowest controller; 0 = none connected)
// Output report (2 bytes): rumble amplitude for the left, right controller (0 = off).

pub const REPORT_LEN: usize = 14;
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
    0x05, 0x06,       //   Usage Page (Generic Device Controls)
    0x09, 0x20,       //   Usage (Battery Strength)
    0x15, 0x00,       //   Logical Minimum (0)
    0x25, 0x64,       //   Logical Maximum (100)
    0x75, 0x08,       //   Report Size (8)
    0x95, 0x01,       //   Report Count (1)
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
pub const NEUTRAL: [u8; REPORT_LEN] = [0, 0, 0x08, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

pub fn hid_report(p: &Pad) -> [u8; REPORT_LEN] {
    let mut r = NEUTRAL;
    r[0..2].copy_from_slice(&p.buttons.to_le_bytes());
    r[3] = p.lt;
    r[4] = p.rt;
    for (i, v) in [p.lx, p.ly, p.rx, p.ry].iter().enumerate() {
        r[5 + 2 * i..7 + 2 * i].copy_from_slice(&v.to_le_bytes());
    }
    r[13] = p.battery.unwrap_or(0);
    r
}

/// Host-provided HID report (bridge CMD_PAD_SET); disables the link feed until the link restarts.
pub static OVERRIDE: Signal<CriticalSectionRawMutex, [u8; REPORT_LEN]> = Signal::new();

pub fn set(report: &[u8]) -> bool {
    if report.len() < REPORT_LEN - 1 {
        return false;
    }
    AUTO.store(false, Ordering::Relaxed);
    let mut r = NEUTRAL;
    let n = report.len().min(REPORT_LEN);
    r[..n].copy_from_slice(&report[..n]);
    OVERRIDE.signal(r);
    true
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

// ---- per-controller state, fed from the radio link ----

const ANALOG_MAX: u16 = 4095;
const STICK_FULL: i32 = 32000;

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
    battery: Option<u8>,
    imu: Imu,
    /// Raw touch levels (ntf 7): X / A, Y / B, stick, trigger.
    levels: [u8; 4],
    /// Thumb down on the touchpad sensors chosen in settings (from `levels`, with hysteresis).
    thumb: bool,
    /// When the thumb last left the touchpad sensors (ms since boot), for `touch_hold_10ms`.
    thumb_off_ms: Option<u64>,
    /// Controller status flags (ntf 6): `STATUS_DEAD_BATTERY`, 0x10 standby.
    status: u8,
}

const STATUS_DEAD_BATTERY: u8 = 0x01;

const IDLE: Hand = Hand {
    side: None,
    buttons: 0,
    touch: 0,
    stick: (0, 0),
    trigger: 0,
    grip: 0,
    battery: None,
    imu: Imu { accel: [0; 3], gyro: [0; 3], ts: 0 },
    levels: [0; 4],
    thumb: false,
    thumb_off_ms: None,
    status: 0,
};

/// The controller's own touch thresholds (ntf 5 bits 0..3) for X / A, Y / B, stick, trigger, in
/// raw level units. `touch_pct` scales them; the default (60 %) is about the controller's own "near"
/// level, which catches a thumb on the rest between the face buttons.
const TOUCH_LEVEL: [u16; 4] = [25, 20, 45, 100];

/// Whether `side`'s thumb is on the touchpad sensors chosen in settings. Once on, it stays on
/// until the level falls below 3/4 of the threshold, so a thumb at the edge doesn't flicker.
fn thumb_on(levels: [u8; 4], side: Side, was_on: bool) -> bool {
    let s = settings::get();
    let mask = if side == Side::Left { s.touch_left } else { s.touch_right };
    (0..4).any(|i| {
        let on = TOUCH_LEVEL[i] * s.touch_pct as u16 / 100;
        let threshold = if was_on { on * 3 / 4 } else { on };
        mask & 1 << i != 0 && levels[i] as u16 > threshold
    })
}

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

/// Battery % of the controller in `slot` (for low-battery warnings).
pub fn battery(slot: u8) -> Option<u8> {
    hands()[slot as usize].battery
}

/// Status flags (ntf 6) of the controller in `slot`.
pub fn status(slot: u8) -> u8 {
    hands()[slot as usize].status
}

/// Whether `slot` is the controller whose IMU feeds the DS4 gyro.
fn is_gyro_source(slot: usize) -> bool {
    let want = if settings::get().gyro_left { Side::Left } else { Side::Right };
    match hands()[slot].side {
        Some(s) if s == want => true,
        // Only one controller connected: use it whichever hand it is.
        Some(_) => !hands().iter().any(|h| h.side == Some(want)),
        None => false,
    }
}

/// One notification payload (the entries after the 4-byte log item).
pub fn link_notifications(slot: u8, mut buf: &[u8]) {
    let slot = slot as usize;
    let gyro_source = is_gyro_source(slot);
    let h = &mut hands()[slot];
    let mut changed = false;
    while buf.len() >= 2 {
        let id = buf[0] & 0x1F;
        let len = ((buf[0] >> 5) | ((buf[1] & 0x0F) << 3)) as usize;
        if id == 0 && len == 0 || buf.len() < 2 + len {
            break;
        }
        let d = &buf[2..2 + len];
        let i16_at = |i: usize| i16::from_le_bytes([d[i], d[i + 1]]);
        match id {
            0 if len >= 1 => {
                changed |= h.battery != Some(d[0]);
                h.battery = Some(d[0].min(100));
            }
            1 if len >= 18 => {
                let mut accel = [i16_at(6), i16_at(8), i16_at(10)];
                let mut gyro = [i16_at(12), i16_at(14), i16_at(16)];
                if h.side == Some(Side::Left) {
                    accel = left_to_right_frame(accel);
                    gyro = left_to_right_frame(gyro);
                }
                h.imu = Imu { ts: u32::from_le_bytes([d[0], d[1], d[2], d[3]]), accel, gyro };
                if h.imu.gyro.iter().any(|g| (*g as i32).abs() > STILL_GYRO) {
                    activity()[slot].moving = true;
                }
                changed |= gyro_source;
            }
            2 if len >= 4 => {
                h.stick = (i16_at(0), i16_at(2));
                changed = true;
            }
            3 if len >= 3 => {
                let t = d[0] as u16 | ((d[1] as u16 & 0x0F) << 8);
                let g = (d[1] as u16 >> 4) | ((d[2] as u16) << 4);
                h.trigger = ANALOG_MAX - t.min(ANALOG_MAX);
                h.grip = ANALOG_MAX - g.min(ANALOG_MAX);
                changed = true;
            }
            4 if len >= 1 => {
                h.buttons = d[0];
                changed = true;
            }
            5 if len >= 1 => h.touch = d[0],
            6 if len >= 1 => h.status = d[0],
            7 if len >= 4 => {
                h.levels = [d[0], d[1], d[2], d[3]];
                if let Some(side) = h.side {
                    let on = thumb_on(h.levels, side, h.thumb);
                    if on != h.thumb {
                        if !on {
                            h.thumb_off_ms = Some(Instant::now().as_millis());
                        }
                        h.thumb = on;
                        changed = true;
                    }
                }
            }
            _ => {}
        }
        buf = &buf[2 + len..];
    }
    if changed {
        publish();
    }
}

/// The left controller's IMU sits rotated 90° about Z from the right's (measured at rest on a table:
/// left X = right Y, left Y = -right X). Turn its samples into the right controller's axes so one
/// `imu_map` (measured on the right) fits both.
fn left_to_right_frame(v: [i16; 3]) -> [i16; 3] {
    [v[1].saturating_neg(), v[0], v[2]]
}

/// Radial deadzone, rescaled so output starts at 0 at its edge. Flips y to HID (+y down).
fn stick((x, y): (i16, i16), deadzone: i32) -> (i16, i16) {
    let (x, y, dz, full) = (x as i64, -(y as i64), deadzone as i64, STICK_FULL as i64);
    let r = (x * x + y * y).isqrt();
    if r <= dz {
        return (0, 0);
    }
    let scale = |v: i64| (v * 32767 * (r.min(full) - dz) / ((full - dz) * r)) as i16;
    (scale(x), scale(y))
}

fn trigger(raw: u16) -> u8 {
    (raw as u32 * 255 / ANALOG_MAX as u32) as u8
}

/// Combine all hands: left Touch = left half of an Xbox pad, right Touch = right half; or, with
/// only one controller connected and single mode on, that controller as a whole mini pad.
fn build() -> Pad {
    let s = settings::get();
    let deadzone = STICK_FULL * s.deadzone_pct as i32 / 100;
    let grip_on = (ANALOG_MAX as u32 * s.grip_pct as u32 / 100) as u16;
    let mut p = Pad::default();
    let connected = hands().iter().filter(|h| h.side.is_some()).count();
    p.connected = connected as u8;
    for (slot, h) in hands().iter().enumerate() {
        let Some(side) = h.side else { continue };
        let b = h.buttons;
        let (x, y) = stick(h.stick, deadzone);
        if connected == 1 && s.single_mode {
            // Mini pad: stick = left stick, face = A / B, trigger = RT, grip = LT, menu = Start.
            for (bit, out) in [(0x01, BTN_A), (0x02, BTN_B), (0x04, BTN_LS), (0x08, BTN_START)] {
                if b & bit != 0 {
                    p.buttons |= out;
                }
            }
            p.rt = trigger(h.trigger);
            p.lt = trigger(h.grip);
            p.lx = x;
            p.ly = y;
        } else {
            let (face0, face1, click, menu, bumper) = match side {
                Side::Left => (BTN_X, BTN_Y, BTN_LS, BTN_START, BTN_LB),
                Side::Right => (BTN_A, BTN_B, BTN_RS, BTN_GUIDE, BTN_RB),
            };
            for (bit, out) in [(0x01, face0), (0x02, face1), (0x04, click), (0x08, menu)] {
                if b & bit != 0 {
                    p.buttons |= out;
                }
            }
            if h.grip > grip_on {
                p.buttons |= bumper;
            }
            if side == Side::Left {
                p.lt = trigger(h.trigger);
                p.lx = x;
                p.ly = y;
            } else {
                p.rt = trigger(h.trigger);
                p.rx = x;
                p.ry = y;
            }
        }
        let hold_ms = s.touch_hold_10ms as u64 * 10;
        let held = h.thumb_off_ms.is_some_and(|t| Instant::now().as_millis() - t < hold_ms);
        if h.thumb || held {
            p.thumbs |= if side == Side::Left { 1 } else { 2 };
        }
        if let Some(bat) = h.battery {
            p.battery = Some(p.battery.map_or(bat, |m| m.min(bat)));
        }
        if is_gyro_source(slot) {
            p.imu = h.imu;
        }
    }
    p
}

fn publish() {
    if AUTO.load(Ordering::Relaxed) {
        PAD.signal(build());
    }
}

// ---- power policy ----

/// Gyro magnitude (per axis, ~0.122 dps/LSB) below which a controller counts as at rest (~5 dps).
const STILL_GYRO: i32 = 40;
/// Frames (2 ms) a controller must be at rest (buttons released) before an armed power-off fires,
/// so the motion of putting it down doesn't wake it straight back up.
const STILL_FRAMES: u32 = 500;
/// An armed power-off fires anyway after this long (30 s).
const ARMED_MAX_FRAMES: u32 = 30 * 500;
const FRAMES_PER_S: u32 = 500;
/// Mode gesture: Oculus / Menu held with the trigger pulled all the way, for this long. It works in
/// every mode, so XInput mode (no serial port for the control panel) can be left without a button.
const MODE_HOLD_S: u32 = 5;
const MODE_TRIGGER: u16 = ANALOG_MAX * 9 / 10;

#[derive(Clone, Copy)]
struct Activity {
    last_buttons: u8,
    hold_since: Option<u32>,
    mode_hold_since: Option<u32>,
    last_active: u32,
    /// Set by any IMU sample above STILL_GYRO since the last policy check.
    moving: bool,
    still_since: u32,
    armed_at: Option<u32>,
    low_batt_warned: bool,
    dead_batt_warned: bool,
}

const NO_ACTIVITY: Activity = Activity {
    last_buttons: 0,
    hold_since: None,
    mode_hold_since: None,
    last_active: 0,
    moving: false,
    still_since: 0,
    armed_at: None,
    low_batt_warned: false,
    dead_batt_warned: false,
};
static mut ACTIVITY: [Activity; 5] = [NO_ACTIVITY; 5];

fn activity() -> &'static mut [Activity; 5] {
    unsafe { &mut *core::ptr::addr_of_mut!(ACTIVITY) }
}

#[derive(PartialEq)]
pub enum PowerAction {
    None,
    /// Power this controller off now.
    OffThis,
    /// Off gesture held: arm every controller (and buzz to confirm).
    ArmAll,
    /// Battery just dropped to the warning level: buzz this controller.
    LowBattery,
    /// This controller reports a dead battery and is shutting down: buzz the others.
    DeadBattery,
    /// Mode gesture held: switch to the next USB mode (and buzz this controller to confirm).
    NextMode,
}

/// Arm a power-off for `slot`: it fires once the controller is put down and still.
pub fn arm_power_off(slot: u8, frame: u32) {
    let a = &mut activity()[slot as usize];
    a.armed_at = Some(frame);
    a.still_since = frame;
}

fn active(h: &Hand) -> bool {
    h.buttons & 0x0F != 0
        || h.touch & 0x0F != 0 // thumb / finger resting on X Y stick trigger
        || h.stick != (0, 0)
        || h.trigger > ANALOG_MAX / 10
        || h.grip > ANALOG_MAX / 10
}

/// Called after each notification batch from `slot` at link frame `frame`.
pub fn power_policy(slot: u8, frame: u32) -> PowerAction {
    let s = settings::get();
    let h = hands()[slot as usize];
    let a = &mut activity()[slot as usize];
    if h.buttons != a.last_buttons || active(&h) {
        a.last_active = frame;
    }
    a.last_buttons = h.buttons;
    if a.moving || h.buttons & 0x0F != 0 {
        a.still_since = frame;
    }
    a.moving = false;
    let menu = h.buttons & 0x08 != 0;
    let mode_gesture = menu && h.trigger >= MODE_TRIGGER;
    if mode_gesture {
        let since = *a.mode_hold_since.get_or_insert(frame);
        if frame.wrapping_sub(since) >= MODE_HOLD_S * FRAMES_PER_S {
            a.mode_hold_since = None;
            return PowerAction::NextMode;
        }
    } else {
        a.mode_hold_since = None;
    }
    if let Some(armed) = a.armed_at {
        let still = frame.wrapping_sub(a.still_since) >= STILL_FRAMES;
        if still || frame.wrapping_sub(armed) >= ARMED_MAX_FRAMES {
            a.armed_at = None;
            return PowerAction::OffThis;
        }
        return PowerAction::None;
    }
    if menu && !mode_gesture && s.off_hold_s > 0 {
        let since = *a.hold_since.get_or_insert(frame);
        if frame.wrapping_sub(since) >= s.off_hold_s as u32 * FRAMES_PER_S {
            a.hold_since = None;
            return PowerAction::ArmAll;
        }
    } else {
        a.hold_since = None;
    }
    if s.idle_min > 0 && frame.wrapping_sub(a.last_active) >= s.idle_min as u32 * 60 * FRAMES_PER_S {
        a.last_active = frame;
        return PowerAction::OffThis;
    }
    if h.status & STATUS_DEAD_BATTERY != 0 && !a.dead_batt_warned {
        a.dead_batt_warned = true;
        return PowerAction::DeadBattery;
    }
    if let Some(bat) = h.battery {
        if !a.low_batt_warned && s.low_batt_pct > 0 && bat <= s.low_batt_pct {
            a.low_batt_warned = true;
            return PowerAction::LowBattery;
        }
    }
    PowerAction::None
}

/// Reset activity tracking for a newly joined controller.
pub fn power_reset(slot: u8, frame: u32) {
    activity()[slot as usize] = Activity { last_active: frame, still_since: frame, ..NO_ACTIVITY };
}
