//! Dongle front panel: RGB status LED, the user button, and pairing from the dongle itself.
//!
//! LED (PCA10059 LED2, active low): red = not paired; fast blue blink = pairing (waiting for a
//! controller in pairing mode); mode colour slow pulse = waiting for controllers, solid = connected
//! (HID green, DS4 blue, XInput purple); triple green / red flash = pairing worked / failed.
//! Button (SW1): press = pair a controller (press again to cancel); hold 3 s = next USB mode
//! (reboots); hold 10 s = forget the pairing.

use core::sync::atomic::{AtomicU8, Ordering};

use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::Input;
use embassy_nrf::pwm::{DutyCycle, SimplePwm};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, Instant, Timer};

use crate::radio::{self, Event, EVENTS};
use crate::settings::{self, Mode};
use crate::{link, pairing};

#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum PairState {
    Idle = 0,
    /// Scanning for a controller in pairing mode (Oculus + Y / Oculus + B).
    Waiting = 1,
    Writing = 2,
    Done = 3,
    Failed = 4,
    TimedOut = 5,
    Cancelled = 6,
}

static PAIR_STATE: AtomicU8 = AtomicU8::new(PairState::Idle as u8);
/// Start (or, while pairing, cancel) pairing: from the button or the GUI.
pub static PAIR_REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// One-shot LED feedback.
static FLASH: Signal<CriticalSectionRawMutex, Flash> = Signal::new();

#[derive(Clone, Copy)]
enum Flash {
    Ok,
    Fail,
    Mode(Mode),
}

pub fn pair_state() -> PairState {
    match PAIR_STATE.load(Ordering::Relaxed) {
        1 => PairState::Waiting,
        2 => PairState::Writing,
        3 => PairState::Done,
        4 => PairState::Failed,
        5 => PairState::TimedOut,
        6 => PairState::Cancelled,
        _ => PairState::Idle,
    }
}

fn set_pair_state(s: PairState) {
    PAIR_STATE.store(s as u8, Ordering::Relaxed);
    let _ = EVENTS.try_send(Event::PairStatus(s as u8));
}

const PAIR_TIMEOUT: Duration = Duration::from_secs(90);
/// DM commands (controller SPL): write pairing record / reset to the main app.
const DM_SET_PAIRING: u8 = 0x14 << 1;
const DM_RESET: u8 = 0x15 << 1;
const DM_ERROR: u8 = 0x80;

/// Pair one controller: scan for it in pairing mode, give it our network id + key, reset it into
/// its main app, then resume hosting. Generates (and stores) a network on first use.
async fn pair_once() -> PairState {
    let (p, fresh) = match pairing::load() {
        Some(p) => (p, false),
        None => (pairing::generate(), true),
    };
    radio::DM_LINK.reset();
    radio::dm_start(0, 0, true);
    set_pair_state(PairState::Waiting);
    let linked = async {
        loop {
            if radio::DM_LINK.wait().await {
                return;
            }
        }
    };
    match select(with_timeout(PAIR_TIMEOUT, linked), PAIR_REQUEST.wait()).await {
        Either::First(Ok(())) => {}
        Either::First(Err(_)) => return PairState::TimedOut,
        Either::Second(()) => return PairState::Cancelled,
    }
    set_pair_state(PairState::Writing);
    Timer::after(Duration::from_millis(50)).await;
    let mut rec = [0u8; 20];
    rec[..4].copy_from_slice(&p.network_id.to_le_bytes());
    rec[4..].copy_from_slice(&p.key);
    let mut written = false;
    for _ in 0..5 {
        radio::DM_RESP.reset();
        if !radio::dm_send(DM_SET_PAIRING, &rec) {
            Timer::after(Duration::from_millis(50)).await;
            continue;
        }
        if let Ok(hdr) = with_timeout(Duration::from_secs(2), radio::DM_RESP.wait()).await {
            written = hdr & DM_ERROR == 0;
            break;
        }
    }
    if !written {
        return PairState::Failed;
    }
    // The controller resets around the time it acks; the reply may never come.
    if radio::dm_send(DM_RESET, &[]) {
        let _ = with_timeout(Duration::from_millis(500), radio::DM_RESP.wait()).await;
    }
    if fresh {
        radio::stop();
        pairing::store(&p);
    }
    PairState::Done
}

#[embassy_executor::task]
pub async fn pair_task() -> ! {
    loop {
        PAIR_REQUEST.wait().await;
        let result = pair_once().await;
        set_pair_state(result);
        // Back to hosting the main link (the new controller joins it right away).
        radio::stop();
        if let Some(p) = pairing::load() {
            link::start(p.network_id, &p.key, link::SESSION);
        }
        FLASH.signal(if result == PairState::Done { Flash::Ok } else { Flash::Fail });
    }
}

#[embassy_executor::task]
pub async fn button_task(mut button: Input<'static>) -> ! {
    loop {
        button.wait_for_low().await;
        let pressed = Instant::now();
        // Feedback while holding: mode colour at 3 s, red at 10 s.
        let mut held = Duration::from_ticks(0);
        let mut shown_mode = false;
        while button.is_low() {
            Timer::after(Duration::from_millis(20)).await;
            held = Instant::now() - pressed;
            if !shown_mode && held >= Duration::from_secs(3) {
                shown_mode = true;
                FLASH.signal(Flash::Mode(settings::get().mode.next()));
            }
        }
        if held >= Duration::from_secs(10) {
            radio::stop();
            pairing::forget();
            FLASH.signal(Flash::Fail);
            Timer::after(Duration::from_millis(800)).await;
            cortex_m::peripheral::SCB::sys_reset();
        } else if held >= Duration::from_secs(3) {
            let mut s = settings::get();
            s.mode = s.mode.next();
            set_mode_and_reboot(s).await;
        } else if held >= Duration::from_millis(30) {
            PAIR_REQUEST.signal(());
        }
    }
}

/// Persist a new USB mode and reboot into it.
pub async fn set_mode_and_reboot(s: settings::Settings) -> ! {
    radio::stop();
    settings::store(&s);
    FLASH.signal(Flash::Mode(s.mode));
    Timer::after(Duration::from_millis(600)).await;
    cortex_m::peripheral::SCB::sys_reset()
}

fn mode_color(m: Mode) -> (u16, u16, u16) {
    match m {
        // Green is much brighter than red / blue at the same duty: scale it down.
        Mode::Hid => (0, 450, 0),
        Mode::Ds4 => (0, 0, 1000),
        Mode::XInput => (1000, 0, 1000),
    }
}

#[embassy_executor::task]
pub async fn led_task(mut pwm: SimplePwm<'static>) -> ! {
    let mut t: u32 = 0;
    loop {
        let s = settings::get();
        let level = s.led_level as u32;
        let set = |pwm: &mut SimplePwm<'static>, (r, g, b): (u16, u16, u16), on: bool| {
            let k = |c: u16| if on { (c as u32 * level / 255) as u16 } else { 0 };
            pwm.set_duty(0, DutyCycle::normal(k(r)));
            pwm.set_duty(1, DutyCycle::normal(k(g)));
            pwm.set_duty(2, DutyCycle::normal(k(b)));
        };
        if let Some(f) = FLASH.try_take() {
            let (color, n) = match f {
                Flash::Ok => ((0, 450, 0), 3),
                Flash::Fail => ((1000, 0, 0), 3),
                Flash::Mode(m) => (mode_color(m), 2),
            };
            for _ in 0..n {
                set(&mut pwm, color, true);
                Timer::after(Duration::from_millis(150)).await;
                set(&mut pwm, color, false);
                Timer::after(Duration::from_millis(150)).await;
            }
        }
        let connected = !link::slots().is_empty();
        let (color, on) = if pair_state() == PairState::Waiting || pair_state() == PairState::Writing {
            ((0, 0, 1000), t % 10 < 5)
        } else if pairing::load().is_none() {
            ((1000, 0, 0), true)
        } else if connected {
            (mode_color(s.mode), true)
        } else {
            (mode_color(s.mode), t % 75 < 15)
        };
        set(&mut pwm, color, on);
        t = t.wrapping_add(1);
        Timer::after(Duration::from_millis(20)).await;
    }
}
