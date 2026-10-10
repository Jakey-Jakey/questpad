//! Pairing record (network id + link key) kept in the last application flash page, so the
//! dongle can host the link on its own from power-up.
//!
//! Self-update only erases the pages the new image occupies (from 0x1000), and memory.x stops
//! the image below this page, so the record survives reflashing (a bootloader DFU may wipe it).

use core::ptr::{read_volatile, write_volatile};

const PAGE: u32 = 0xDF000;
const MAGIC: u32 = 0x3152_5051; // "QPR1"
const WORDS: usize = 7; // magic, network_id, key[16], crc

const NVMC_READY: u32 = 0x4001_E400;
const NVMC_CONFIG: u32 = 0x4001_E504;
const NVMC_ERASEPAGE: u32 = 0x4001_E508;

#[derive(Clone, Copy)]
pub struct Pairing {
    pub network_id: u32,
    pub key: [u8; 16],
}

fn words(p: &Pairing) -> [u32; WORDS] {
    let mut w = [0u32; WORDS];
    w[0] = MAGIC;
    w[1] = p.network_id;
    for i in 0..4 {
        w[2 + i] = u32::from_le_bytes([p.key[4 * i], p.key[4 * i + 1], p.key[4 * i + 2], p.key[4 * i + 3]]);
    }
    w[6] = checksum(&w[..6]);
    w
}

fn checksum(w: &[u32]) -> u32 {
    w.iter().fold(0x811C_9DC5u32, |h, &x| (h ^ x).wrapping_mul(0x0100_0193))
}

pub fn load() -> Option<Pairing> {
    let mut w = [0u32; WORDS];
    for (i, x) in w.iter_mut().enumerate() {
        *x = unsafe { read_volatile((PAGE + 4 * i as u32) as *const u32) };
    }
    if w[0] != MAGIC || w[6] != checksum(&w[..6]) {
        return None;
    }
    let mut key = [0u8; 16];
    for i in 0..4 {
        key[4 * i..4 * i + 4].copy_from_slice(&w[2 + i].to_le_bytes());
    }
    Some(Pairing { network_id: w[1], key })
}

fn wait_ready() {
    while unsafe { read_volatile(NVMC_READY as *const u32) } == 0 {}
}

/// Erase the page and write the record. The CPU stalls for the ~85 ms erase, so callers
/// stop the radio first.
pub fn store(p: &Pairing) -> bool {
    let w = words(p);
    critical_section::with(|_| unsafe {
        write_volatile(NVMC_CONFIG as *mut u32, 2);
        write_volatile(NVMC_ERASEPAGE as *mut u32, PAGE);
        wait_ready();
        write_volatile(NVMC_CONFIG as *mut u32, 1);
        for (i, &x) in w.iter().enumerate() {
            write_volatile((PAGE + 4 * i as u32) as *mut u32, x);
            wait_ready();
        }
        write_volatile(NVMC_CONFIG as *mut u32, 0);
    });
    load().is_some_and(|q| q.network_id == p.network_id && q.key == p.key)
}
