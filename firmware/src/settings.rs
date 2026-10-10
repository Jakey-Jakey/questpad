//! User settings, kept in flash page 0xDE000 (next to the pairing page) and mirrored in RAM.

use core::cell::Cell;
use core::ptr::{read_volatile, write_volatile};

use critical_section::Mutex;

const PAGE: u32 = 0xDE000;
const MAGIC: u32 = 0x3153_5051; // "QPS1"
pub const LEN: usize = 15;

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
    // Measured on the right controller (NOTES.md): held naturally, Touch -Z is up and -Y points
    // forward, so DS4 x = +X, y = -Z, z = +Y.
    imu_map: [0x00, 0x82, 0x01],
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
        ]
    }

    pub fn from_bytes(b: &[u8]) -> Option<Settings> {
        if b.len() < LEN {
            return None;
        }
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

/// Flash layout: [magic][data words][checksum].
const DATA_WORDS: usize = (LEN + 3) / 4;
const WORDS: usize = DATA_WORDS + 2;

fn pack(s: &Settings) -> [u32; WORDS] {
    let mut w = [0u32; WORDS];
    w[0] = MAGIC;
    for (i, chunk) in s.to_bytes().chunks(4).enumerate() {
        let mut x = [0u8; 4];
        x[..chunk.len()].copy_from_slice(chunk);
        w[1 + i] = u32::from_le_bytes(x);
    }
    w[WORDS - 1] = checksum(&w[..WORDS - 1]);
    w
}

/// Load settings from flash into RAM (defaults if the page is blank or corrupt).
pub fn load() -> Settings {
    let mut w = [0u32; WORDS];
    for (i, x) in w.iter_mut().enumerate() {
        *x = unsafe { read_volatile((PAGE + 4 * i as u32) as *const u32) };
    }
    let s = if w[0] == MAGIC && w[WORDS - 1] == checksum(&w[..WORDS - 1]) {
        let mut b = [0u8; 4 * DATA_WORDS];
        for i in 0..DATA_WORDS {
            b[4 * i..4 * i + 4].copy_from_slice(&w[1 + i].to_le_bytes());
        }
        Settings::from_bytes(&b).unwrap_or(DEFAULT)
    } else {
        DEFAULT
    };
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
