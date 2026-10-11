//! User settings, kept in flash page 0xDE000 (next to the pairing page) and mirrored in RAM.

use core::cell::Cell;
use core::ptr::{read_volatile, write_volatile};

use critical_section::Mutex;

const PAGE: u32 = 0xDE000;
const MAGIC: u32 = 0x3253_5051; // "QPS2": [magic][length][data words][checksum]
/// Firmware before 1.1.0: [magic][data words][checksum], `OLD_LENS` bytes (distinct word counts).
const MAGIC_V1: u32 = 0x3153_5051; // "QPS1"
/// Longest record `load` accepts (a later firmware's layout; extra bytes are ignored).
const MAX_LEN: usize = 64;
pub const LEN: usize = 19;
/// Older layouts, still read from flash: 15 bytes (1.0.x, no touchpad settings) and 18 (a 1.1.0
/// pre-release).
/// Shorter records (over USB too) get defaults for the missing settings.
const OLD_LENS: [usize; 2] = [18, 15];

const NVMC_READY: u32 = 0x4001_E400;
const NVMC_CONFIG: u32 = 0x4001_E504;
const NVMC_ERASEPAGE: u32 = 0x4001_E508;

/// How the dongle presents itself over USB.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Generic HID gamepad + serial port (default; works everywhere, needs a Steam layout once).
    Hid = 0,
    /// DualShock 4 (Steam / SDL / macOS know it: gyro, rumble, battery, no setup).
    Ds4 = 1,
    /// Wired Xbox 360 (XInput on Windows, `xpad` on Linux). No serial port in this mode.
    XInput = 2,
}

impl Mode {
    fn from(v: u8) -> Mode {
        match v {
            1 => Mode::Ds4,
            2 => Mode::XInput,
            _ => Mode::Hid,
        }
    }
    pub fn next(self) -> Mode {
        Mode::from((self as u8 + 1) % 3)
    }
}

/// Serialized as `LEN` bytes (bridge 0x29 / 0x2A use the same layout).
#[derive(Clone, Copy, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    /// Extra stick deadzone, % of full scale (the controllers already report 0,0 at rest).
    pub deadzone_pct: u8,
    /// Grip squeeze (%) that counts as LB / RB.
    pub grip_pct: u8,
    /// Minutes without input before a controller powers off (0 = never).
    pub idle_min: u8,
    /// Seconds to hold Oculus / Menu for the power-off gesture (0 = gesture off).
    pub off_hold_s: u8,
    /// Short buzz when a controller connects.
    pub connect_buzz: bool,
    /// Buzz when a controller's battery drops to this % (0 = off).
    pub low_batt_pct: u8,
    /// With only one controller connected, map it as a whole mini gamepad.
    pub single_mode: bool,
    /// Which controller's IMU feeds the DS4 gyro: false = right, true = left.
    pub gyro_left: bool,
    /// Haptic strength for confirmation buzzes (0..255).
    pub buzz_amp: u8,
    /// Status LED brightness (0 = off .. 255).
    pub led_level: u8,
    /// Power the controllers off when the computer sleeps.
    pub off_on_suspend: bool,
    /// Controller IMU -> DS4 axes, for gyro x / y / z (accel uses the same map): bits 0..1 =
    /// source axis, bit 7 = negate.
    pub imu_map: [u8; 3],
    /// DS4 touchpad: which touch sensors put the left / right thumb on its half of the touchpad.
    /// Bits 0 X / A, 1 Y / B, 2 stick (0 = never).
    pub touch_left: u8,
    pub touch_right: u8,
    /// Both thumbs down = a two-finger touch. Off: only the gyro controller's thumb is reported.
    pub touch_both: bool,
    /// A thumb still counts as down this long after leaving the sensors (10 ms units): they drop
    /// out briefly while the thumb shifts during play, which would pause pad-touch gyro.
    pub touch_hold_10ms: u8,
}

pub const DEFAULT: Settings = Settings {
    mode: Mode::Hid,
    deadzone_pct: 3,
    grip_pct: 50,
    idle_min: 10,
    off_hold_s: 3,
    connect_buzz: true,
    low_batt_pct: 10,
    single_mode: true,
    gyro_left: false,
    buzz_amp: 200,
    led_level: 40,
    off_on_suspend: true,
    // Measured on the right controller (PROTOCOL.md): held naturally, Touch -Z is up and -Y points
    // forward, so DS4 x = +X, y = -Z, z = +Y. Left samples are first turned into these axes (gamepad.rs).
    imu_map: [0x00, 0x82, 0x01],
    touch_left: 0x07,
    touch_right: 0x07,
    touch_both: false,
    touch_hold_10ms: 15,
};

impl Settings {
    pub fn to_bytes(&self) -> [u8; LEN] {
        [
            self.mode as u8,
            self.deadzone_pct,
            self.grip_pct,
            self.idle_min,
            self.off_hold_s,
            self.connect_buzz as u8,
            self.low_batt_pct,
            self.single_mode as u8,
            self.gyro_left as u8,
            self.buzz_amp,
            self.led_level,
            self.off_on_suspend as u8,
            self.imu_map[0],
            self.imu_map[1],
            self.imu_map[2],
            self.touch_left,
            self.touch_right,
            self.touch_both as u8,
            self.touch_hold_10ms,
        ]
    }

    /// Parse `LEN` bytes, or an older (shorter) layout with defaults for the rest.
    pub fn from_bytes(b: &[u8]) -> Option<Settings> {
        if b.len() < OLD_LENS[OLD_LENS.len() - 1] {
            return None;
        }
        let mut full = DEFAULT.to_bytes();
        let n = b.len().min(LEN);
        full[..n].copy_from_slice(&b[..n]);
        let b = &full;
        Some(Settings {
            mode: Mode::from(b[0]),
            deadzone_pct: b[1].min(50),
            grip_pct: b[2].clamp(5, 95),
            idle_min: b[3],
            off_hold_s: b[4].min(20),
            connect_buzz: b[5] != 0,
            low_batt_pct: b[6].min(50),
            single_mode: b[7] != 0,
            gyro_left: b[8] != 0,
            buzz_amp: b[9],
            led_level: b[10],
            off_on_suspend: b[11] != 0,
            imu_map: [b[12] & 0x83, b[13] & 0x83, b[14] & 0x83],
            touch_left: b[15] & 0x07,
            touch_right: b[16] & 0x07,
            touch_both: b[17] != 0,
            touch_hold_10ms: b[18],
        })
    }
}

static CURRENT: Mutex<Cell<Settings>> = Mutex::new(Cell::new(DEFAULT));

pub fn get() -> Settings {
    critical_section::with(|cs| CURRENT.borrow(cs).get())
}

fn checksum(w: &[u32]) -> u32 {
    w.iter().fold(0x811C_9DC5u32, |h, &x| (h ^ x).wrapping_mul(0x0100_0193))
}

const DATA_WORDS: usize = (LEN + 3) / 4;
const WORDS: usize = DATA_WORDS + 3;

fn pack(s: &Settings) -> [u32; WORDS] {
    let mut w = [0u32; WORDS];
    w[0] = MAGIC;
    w[1] = LEN as u32;
    for (i, chunk) in s.to_bytes().chunks(4).enumerate() {
        let mut x = [0u8; 4];
        x[..chunk.len()].copy_from_slice(chunk);
        w[2 + i] = u32::from_le_bytes(x);
    }
    w[WORDS - 1] = checksum(&w[..WORDS - 1]);
    w
}

fn flash_word(i: usize) -> u32 {
    unsafe { read_volatile((PAGE + 4 * i as u32) as *const u32) }
}

/// The data bytes of a valid record: `magic`, `header` words in all, `data_words`, checksum.
fn record(magic: u32, header: usize, data_words: usize) -> Option<[u8; MAX_LEN]> {
    let mut w = [0u32; MAX_LEN / 4 + 3];
    let w = &mut w[..header + data_words + 1];
    for (i, x) in w.iter_mut().enumerate() {
        *x = flash_word(i);
    }
    let last = w.len() - 1;
    if w[0] != magic || w[last] != checksum(&w[..last]) {
        return None;
    }
    let mut b = [0u8; MAX_LEN];
    for i in 0..data_words {
        b[4 * i..4 * i + 4].copy_from_slice(&w[header + i].to_le_bytes());
    }
    Some(b)
}

fn stored() -> Option<Settings> {
    if flash_word(0) == MAGIC {
        let len = flash_word(1) as usize;
        if !(1..=MAX_LEN).contains(&len) {
            return None;
        }
        return Settings::from_bytes(&record(MAGIC, 2, (len + 3) / 4)?[..len]);
    }
    OLD_LENS.iter().find_map(|&len| Settings::from_bytes(&record(MAGIC_V1, 1, (len + 3) / 4)?[..len]))
}

/// Load settings from flash into RAM (defaults if the page is blank or corrupt).
pub fn load() -> Settings {
    let s = stored().unwrap_or(DEFAULT);
    critical_section::with(|cs| CURRENT.borrow(cs).set(s));
    s
}

/// Apply and persist. The CPU stalls ~85 ms for the page erase: callers stop the radio first.
pub fn store(s: &Settings) -> bool {
    critical_section::with(|cs| CURRENT.borrow(cs).set(*s));
    let w = pack(s);
    critical_section::with(|_| unsafe {
        write_volatile(NVMC_CONFIG as *mut u32, 2);
        write_volatile(NVMC_ERASEPAGE as *mut u32, PAGE);
        while read_volatile(NVMC_READY as *const u32) == 0 {}
        write_volatile(NVMC_CONFIG as *mut u32, 1);
        for (i, &x) in w.iter().enumerate() {
            write_volatile((PAGE + 4 * i as u32) as *mut u32, x);
            while read_volatile(NVMC_READY as *const u32) == 0 {}
        }
        write_volatile(NVMC_CONFIG as *mut u32, 0);
    });
    load() == *s
}

/// Set without persisting (live preview from the GUI).
pub fn set_volatile(s: &Settings) {
    critical_section::with(|cs| CURRENT.borrow(cs).set(*s));
}
