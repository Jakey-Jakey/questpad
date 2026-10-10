//! In-application firmware update without the bootloader.
//!
//! The Nordic open bootloader on the dongle ignores GPREGRET (DFU only via the
//! RESET button) and write-protects its settings page, so the app cannot update the
//! stored image CRC. Instead the host forges 4 bytes in a reserved vector-table slot
//! (offset 0x1C) so the new image has the CRC the bootloader already expects, and we
//! flash it here: stage it in RAM, then run a RAM-resident erase/write routine and reset.
//! If anything goes wrong, the bootloader is untouched and the RESET button recovers.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

const APP_START: u32 = 0x1000;
pub const STAGE_LEN: usize = 160 * 1024;

static mut STAGE: [u32; STAGE_LEN / 4] = [0; STAGE_LEN / 4];
static mut LEN: usize = 0;

fn stage() -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(STAGE) as *mut u8, STAGE_LEN) }
}

pub fn begin(len: usize) -> bool {
    if len > STAGE_LEN || len == 0 {
        return false;
    }
    stage().fill(0xFF);
    unsafe { LEN = len };
    true
}

pub fn data(off: usize, bytes: &[u8]) -> bool {
    let s = stage();
    if off + bytes.len() > unsafe { LEN } {
        return false;
    }
    s[off..off + bytes.len()].copy_from_slice(bytes);
    true
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Verify the staged image and flash it. Only returns on a CRC mismatch.
pub fn commit(len: usize, crc: u32) -> bool {
    if len != unsafe { LEN } || crc32(&stage()[..len]) != crc {
        return false;
    }
    let words = (len + 3) / 4;
    cortex_m::interrupt::disable();
    unsafe { ram_flash(addr_of!(STAGE) as *const u32, words) }
}

/// Runs entirely from RAM (`.data` is copied there at boot): no calls into flash allowed.
#[link_section = ".data.ramfunc"]
#[inline(never)]
unsafe fn ram_flash(src: *const u32, words: usize) -> ! {
    const NVMC_READY: *const u32 = 0x4001_E400 as *const u32;
    const NVMC_CONFIG: *mut u32 = 0x4001_E504 as *mut u32;
    const NVMC_ERASEPAGE: *mut u32 = 0x4001_E508 as *mut u32;
    const AIRCR: *mut u32 = 0xE000_ED0C as *mut u32;

    let end = APP_START + (words as u32) * 4;
    let mut page = APP_START;
    write_volatile(NVMC_CONFIG, 2); // erase enable
    while page < end {
        while read_volatile(NVMC_READY) == 0 {}
        write_volatile(NVMC_ERASEPAGE, page);
        while read_volatile(NVMC_READY) == 0 {}
        page += 4096;
    }
    write_volatile(NVMC_CONFIG, 1); // write enable
    let mut i = 0;
    while i < words {
        write_volatile((APP_START + 4 * i as u32) as *mut u32, read_volatile(src.add(i)));
        while read_volatile(NVMC_READY) == 0 {}
        i += 1;
    }
    write_volatile(NVMC_CONFIG, 0);
    write_volatile(AIRCR, 0x05FA_0004); // SYSRESETREQ
    loop {}
}
