//! Hardware watchdog: if the firmware ever hangs, the dongle resets itself (8 s) instead of
//! needing a replug. Fed from a task; the RAM self-update routine feeds it too.

use core::ptr::write_volatile;

use embassy_time::{Duration, Timer};

const WDT: u32 = 0x4001_0000;
const TIMEOUT_S: u32 = 8;
const RELOAD: u32 = 0x6E52_4635;

pub fn start() {
    unsafe {
        write_volatile((WDT + 0x50C) as *mut u32, 1); // CONFIG: keep running in sleep, pause when debug-halted
        write_volatile((WDT + 0x504) as *mut u32, TIMEOUT_S * 32768); // CRV
        write_volatile((WDT + 0x508) as *mut u32, 1); // RREN: RR[0]
        write_volatile(WDT as *mut u32, 1); // TASKS_START (no-op if already running across a soft reset)
    }
}

pub fn feed() {
    unsafe { write_volatile((WDT + 0x600) as *mut u32, RELOAD) }
}

#[embassy_executor::task]
pub async fn task() -> ! {
    loop {
        feed();
        Timer::after(Duration::from_secs(1)).await;
    }
}
