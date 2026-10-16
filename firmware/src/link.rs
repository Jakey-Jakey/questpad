//! Pulsar main-link host: the dongle plays the headset (syncboss `pulsar_ll_host.c` /
//! `pulsar_cl_host.c`) for paired Touch controllers.
//!
//! One 2000 us frame, times relative to the beacon's on-air start T:
//!   T-40          TXEN (PPI) -> beacon on the hopped channel, prefix 0xF0
//!   T+300         RXEN -> listen on logical addresses 1..5 (= device slots 0..4)
//!   T+344..1781   device uplinks at T+[350,575,875,1175,1475] (slot 0 = join)
//!   T+1790        window closes: drop stale slots, build the next beacon
//! Uplinks are AES-CCM (nRF CCM, BLE-style) with the link key; the IV is the beacon
//! timestamp u48 + session u16, so the timestamp advances by exactly 2000 per frame.
//! Uplinks are queued at RADIO END and decrypted by the CCM interrupt.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use crate::radio::{self, Event, EVENTS};
use radio::{r, w};

const CCM: u32 = 0x4000_F000;
const CCM_TASKS_KSGEN: u32 = CCM;
const CCM_EVENTS_ENDCRYPT: u32 = CCM + 0x104;
const CCM_EVENTS_ERROR: u32 = CCM + 0x108;
const CCM_SHORTS: u32 = CCM + 0x200;
const CCM_MICSTATUS: u32 = CCM + 0x400;
const CCM_ENABLE: u32 = CCM + 0x500;
const CCM_MODE: u32 = CCM + 0x504;
const CCM_CNFPTR: u32 = CCM + 0x508;
const CCM_INPTR: u32 = CCM + 0x50C;
const CCM_OUTPTR: u32 = CCM + 0x510;
const CCM_SCRATCHPTR: u32 = CCM + 0x514;
const CCM_MAXPACKETSIZE: u32 = CCM + 0x518;
const CCM_INTENSET: u32 = CCM + 0x304;
const CCM_INTENCLR: u32 = CCM + 0x308;
const CCM_INT_ENDCRYPT: u32 = 1 << 1;
const CCM_INT_ERROR: u32 = 1 << 2;
/// Decrypt, 2 Mbit, extended (8-bit) length; the controller encrypts with 0x01010000.
const CCM_MODE_DECRYPT: u32 = 0x0101_0001;

const SHORT_END_START: u32 = 1 << 5;

const FRAME_US: u32 = 2000;
const RX_ON: u32 = 300;
const RX_OFF: u32 = 1790;
/// Build/schedule a beacon only if at least this much time is left before its TXEN.
const MIN_LEAD: i32 = 100;

/// Pulsar data channels (BLE data-channel table): idx 0..36 -> FREQUENCY.
const CHANNELS: [u8; 37] = [
    4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62,
    64, 66, 68, 70, 72, 74, 76, 78,
];

const BEACON_PREFIX: u32 = 0xF0;
const S0_PULSAR: u8 = 4;
const SLOTS: usize = 5;
const TL_NOTIFICATION: u8 = 0x40;
const TL_READ: u8 = 0x10;
/// Controller register: simple haptics, 1 byte amplitude (lcon `FUN_0000fbb8`).
const REG_HAPTIC: u8 = 0x97;
/// Max TL request data (syncboss TL_HOST_MAX_PAYLOAD_LEN).
pub const DL_MAX: usize = 32;
/// Controller register: power off (system off; wakes on motion / button), any data.
const REG_POWER_OFF: u8 = 0x05;
/// Feedback buzz lengths (frames of 2 ms): power-off and mode gestures, connect, low battery.
const BUZZ_FRAMES: u32 = 100;
const CONNECT_BUZZ_FRAMES: u32 = 60;
const LOW_BATT_BUZZ_FRAMES: u32 = 400;
/// Beacons a request is repeated for without an echo before giving up (1 s; a controller in
/// standby rarely transmits, so its echo is slow).
const DL_TRIES: u16 = 500;
/// Beacon session id (any value; both ends put it in the IV).
pub const SESSION: u16 = 0x5155;
/// RX buffers: the radio fills one while queued ones wait for the CCM.
const NBUF: usize = 6;
/// Frames without an uplink before a slot is freed (1 s).
const SLOT_TIMEOUT: u32 = 500;
/// Syncboss pads the join-response CL data to this length.
const JOIN_RESP_LEN: usize = 36;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    TxWait,
    Tx,
    RxWait,
    Rx,
}

#[derive(Clone, Copy)]
struct Slot {
    used: bool,
    id: [u8; 8],
    last_frame: u32,
    right: bool,
    /// Request being (re)sent in beacons until the device echoes its seq.
    pending: Option<Down>,
    /// Host-requested register access, sent after `pending`.
    custom: Option<Down>,
    /// Haptic amplitude the device has confirmed (register 0x97).
    haptic_done: u8,
    /// Confirmation buzz overrides the host rumble until this frame.
    buzz_until: u32,
}

/// Downlink TL request: [reg][seq | read << 4][data].
#[derive(Clone, Copy)]
struct Down {
    reg: u8,
    read: bool,
    len: u8,
    data: [u8; DL_MAX],
    seq: u8,
    tries: u16,
    /// Set for a haptics write: amplitude to record as confirmed on ack.
    haptic: Option<u8>,
}

const NO_JOB: Job = Job { buf: 0, slot: 0, rssi: 0, ts: 0 };
const EMPTY_SLOT: Slot = Slot {
    used: false,
    id: [0; 8],
    last_frame: 0,
    right: false,
    pending: None,
    custom: None,
    haptic_done: 0,
    buzz_until: 0,
};

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub frames: u32,
    pub rx_crc_ok: u32,
    pub rx_crc_bad: u32,
    pub mic_ok: u32,
    pub mic_bad: u32,
    pub joins: u32,
    pub late: u32,
    pub ccm_max_us: u32,
    pub dl_ok: u32,
    pub dl_fail: u32,
}

struct Link {
    phase: Phase,
    session: u16,
    hop: u32,
    unmapped: u32,
    /// TIMER0 time of the current beacon's on-air start.
    t: u32,
    /// Timestamp carried by the current beacon (us, 48-bit).
    ts: u64,
    frame: u32,
    slots: [Slot; SLOTS],
    /// Slots heard (MIC ok) this frame; sent as the next beacon's ack mask.
    ack: u8,
    /// Pending join response (device id, slot).
    join: Option<([u8; 8], u8)>,
    cur: usize,
    jobs: heapless::Deque<Job, NBUF>,
    job: Job,
    ccm_busy: bool,
    ccm_t0: u32,
    /// Wanted haptic amplitude for the left / right controller.
    rumble: [u8; 2],
    /// Round-robin start for picking which slot gets this beacon's downlink.
    dl_next: usize,
    /// Last TL request seq (1..15). Devices echo the seq of the last beacon TL frame they saw,
    /// whoever it was for, so seqs are global: an echo of seq N from the target means it got N.
    seq: u8,
    stats: Stats,
}

static mut LINK: Link = Link {
    phase: Phase::TxWait,
    session: 0,
    hop: 5,
    unmapped: 0,
    t: 0,
    ts: 0,
    frame: 0,
    slots: [EMPTY_SLOT; SLOTS],
    ack: 0,
    join: None,
    cur: 0,
    jobs: heapless::Deque::new(),
    job: NO_JOB,
    ccm_busy: false,
    ccm_t0: 0,
    rumble: [0; 2],
    dl_next: 1,
    seq: 0,
    stats: Stats {
        frames: 0,
        rx_crc_ok: 0,
        rx_crc_bad: 0,
        mic_ok: 0,
        mic_bad: 0,
        joins: 0,
        late: 0,
        ccm_max_us: 0,
        dl_ok: 0,
        dl_fail: 0,
    },
};

/// nRF CCM configuration: key[16], pktctr (5 B + 3), direction (byte 24), IV[8] at 25.
#[repr(C, align(4))]
struct CcmCnf([u8; 33]);
static mut CNF: CcmCnf = CcmCnf([0; 33]);
static mut SCRATCH: [u32; 68] = [0; 68];
static mut RX: [[u8; radio::BUF_LEN]; NBUF] = [[0; radio::BUF_LEN]; NBUF];
static mut PLAIN: [u8; radio::BUF_LEN] = [0; radio::BUF_LEN];
static mut BEACON: [u8; 64] = [0; 64];

fn link() -> &'static mut Link {
    unsafe { &mut *addr_of_mut!(LINK) }
}
fn rx_ptr(i: usize) -> u32 {
    unsafe { addr_of!(RX[i]) as u32 }
}
#[inline(always)]
fn poke(addr: u32, v: u32) {
    unsafe { write_volatile(addr as *mut u32, v) }
}
#[inline(always)]
fn peek(addr: u32) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

// ---- thread-mode API ----

/// Start hosting network `network_id` (pairing record) with `key` (link key).
pub fn start(network_id: u32, key: &[u8; 16], session: u16) {
    critical_section::with(|_| {
        radio::take_for_link();
        for s in 1..SLOTS {
            crate::gamepad::link_drop(s as u8);
        }
        crate::gamepad::AUTO.store(true, core::sync::atomic::Ordering::Relaxed);
        let l = link();
        *l = Link {
            phase: Phase::TxWait,
            session,
            hop: (network_id & 0xFF) % 11 + 5,
            unmapped: 0,
            t: 0,
            ts: 1_000_000,
            frame: 0,
            slots: [EMPTY_SLOT; SLOTS],
            ack: 0,
            join: None,
            cur: 0,
            jobs: heapless::Deque::new(),
            job: NO_JOB,
            ccm_busy: false,
            ccm_t0: 0,
            rumble: [0; 2],
            dl_next: 1,
            seq: 0,
            stats: Stats::default(),
        };
        unsafe {
            let cnf = &mut *addr_of_mut!(CNF);
            cnf.0 = [0; 33];
            cnf.0[..16].copy_from_slice(key);
        }
        poke(CCM_INTENCLR, 0xFFFF_FFFF);
        poke(CCM + 0x008, 1); // TASKS_STOP
        poke(CCM_ENABLE, 2);
        poke(CCM_MODE, CCM_MODE_DECRYPT);
        poke(CCM_CNFPTR, addr_of!(CNF) as u32);
        poke(CCM_SCRATCHPTR, addr_of!(SCRATCH) as u32);
        poke(CCM_MAXPACKETSIZE, 0xFB);
        poke(CCM_SHORTS, 1); // ENDKSGEN -> CRYPT

        // Pulsar main-link PHY: [S0][LEN][S1][payload], BASE = network id,
        // logical 0 = beacon (prefix F0), logical 1..5 = device slots 0..4 (prefix slot+1).
        w(radio::MODE, 1);
        w(radio::PCNF0, 0x0010_0108);
        w(radio::PCNF1, 0x0104_0000 | 255);
        w(radio::BASE0, network_id);
        w(radio::BASE1, network_id);
        w(radio::PREFIX0, BEACON_PREFIX | 1 << 8 | 2 << 16 | 3 << 24);
        w(radio::PREFIX1, 4 | 5 << 8);
        w(radio::TXADDRESS, 0);
        w(radio::RXADDRESSES, 0b11_1110);

        unsafe {
            use embassy_nrf::interrupt::{InterruptExt, Priority};
            // Same priority as RADIO/TIMER0, so the three handlers never preempt each other.
            embassy_nrf::interrupt::AAR_CCM.set_priority(Priority::P0);
            embassy_nrf::interrupt::AAR_CCM.unpend();
            embassy_nrf::interrupt::AAR_CCM.enable();
        }
        l.t = radio::now().wrapping_add(5000);
        next_beacon(l);
        schedule_beacon(l);
    })
}

pub fn running() -> bool {
    critical_section::with(|_| radio::eng().mode == radio::Mode::Link)
}

/// Connected devices: (slot, right hand, device id).
pub fn slots() -> heapless::Vec<(u8, bool, [u8; 8]), 4> {
    critical_section::with(|_| {
        let l = link();
        (1..SLOTS).filter(|&s| l.slots[s].used).map(|s| (s as u8, l.slots[s].right, l.slots[s].id)).collect()
    })
}

pub fn stats() -> Stats {
    critical_section::with(|_| link().stats)
}

// ---- frame engine (interrupt context) ----

/// Advance the hop (BLE CSA#1, full channel map) and build the beacon for frame `l.t`.
fn next_beacon(l: &mut Link) {
    l.unmapped = (l.unmapped + l.hop) % 37;
    l.frame = l.frame.wrapping_add(1);
    l.stats.frames += 1;
    let b = unsafe { &mut *addr_of_mut!(BEACON) };
    let p = &mut b[3..];
    // [0..5] flags(3) | channel map(37) << 3, [5] unmapped channel, [6..8] session,
    // [8..14] timestamp u48, [14..] CL data.
    let map = ((1u64 << 37) - 1) << 3;
    p[..5].copy_from_slice(&map.to_le_bytes()[..5]);
    p[5] = l.unmapped as u8;
    p[6..8].copy_from_slice(&l.session.to_le_bytes());
    p[8..14].copy_from_slice(&l.ts.to_le_bytes()[..6]);
    let cl = &mut p[14..];
    let cl_len = if let Some((id, slot)) = l.join.take() {
        // Join response: [01][ack][02 accept][device id 8][slot][iv override 0][1], zero padded.
        cl[..JOIN_RESP_LEN].fill(0);
        cl[0] = 1;
        cl[1] = l.ack;
        cl[2] = 2;
        cl[3..11].copy_from_slice(&id);
        cl[11] = slot;
        cl[12] = 0;
        cl[13] = 1;
        JOIN_RESP_LEN
    } else if let Some((slot, d)) = next_downlink(l) {
        // Request for one device: [1 << slot][ack mask][reg][seq | read << 4][data].
        cl[0] = 1 << slot;
        cl[1] = l.ack;
        cl[2] = d.reg;
        cl[3] = d.seq | if d.read { TL_READ } else { 0 };
        cl[4..4 + d.len as usize].copy_from_slice(&d.data[..d.len as usize]);
        4 + d.len as usize
    } else {
        // Idle: [downlink mask][ack mask].
        cl[0] = 0;
        cl[1] = l.ack;
        2
    };
    l.ack = 0;
    b[0] = S0_PULSAR;
    b[1] = (14 + cl_len) as u8;
    b[2] = 0;
}

/// Pick the slot whose request rides in this beacon (round robin) and count the attempt.
fn next_downlink(l: &mut Link) -> Option<(usize, Down)> {
    for i in 0..SLOTS - 1 {
        let s = 1 + (l.dl_next - 1 + i) % (SLOTS - 1);
        let (rumble, frame) = (l.rumble, l.frame);
        let seq = l.seq % 15 + 1;
        let slot = &mut l.slots[s];
        if !slot.used {
            continue;
        }
        if slot.pending.is_none() {
            let buzzing = (slot.buzz_until.wrapping_sub(frame) as i32) > 0;
            let want = if buzzing { crate::settings::get().buzz_amp } else { rumble[slot.right as usize] };
            let mut next = slot.custom.take();
            if next.is_none() && want != slot.haptic_done {
                let mut d = Down { reg: REG_HAPTIC, read: false, len: 1, data: [0; DL_MAX], seq: 0, tries: 0, haptic: Some(want) };
                d.data[0] = want;
                next = Some(d);
            }
            if let Some(mut d) = next {
                d.seq = seq;
                slot.pending = Some(d);
                l.seq = seq;
            }
        }
        if let Some(d) = slot.pending.as_mut() {
            d.tries += 1;
            let d = *d;
            if d.tries > DL_TRIES {
                slot.pending = None;
                l.stats.dl_fail += 1;
                continue;
            }
            l.dl_next = s % (SLOTS - 1) + 1;
            return Some((s, d));
        }
    }
    None
}

fn queue_power_off(s: &mut Slot) {
    if s.used {
        s.custom = Some(Down { reg: REG_POWER_OFF, read: false, len: 0, data: [0; DL_MAX], seq: 0, tries: 0, haptic: None });
    }
}

fn queue_power_off_all(l: &mut Link) {
    l.rumble = [0; 2];
    for s in l.slots.iter_mut().skip(1) {
        queue_power_off(s);
    }
}

/// Power off every connected controller (they wake on motion or a button and rejoin).
pub fn power_off_all() {
    critical_section::with(|_| {
        if radio::eng().mode == radio::Mode::Link {
            queue_power_off_all(link());
        }
    })
}

/// Set the haptic amplitude (0 = off) of the left and right controllers.
pub fn rumble(left: u8, right: u8) {
    critical_section::with(|_| link().rumble = [left, right])
}

/// Queue a register write (or read, answered in an uplink) for the device in `slot`.
pub fn request(slot: u8, reg: u8, read: bool, data: &[u8]) -> bool {
    critical_section::with(|_| {
        let l = link();
        let Some(s) = l.slots.get_mut(slot as usize).filter(|s| s.used && slot > 0) else {
            return false;
        };
        if s.custom.is_some() || data.len() > DL_MAX {
            return false;
        }
        let mut d = Down { reg, read, len: data.len() as u8, data: [0; DL_MAX], seq: 0, tries: 0, haptic: None };
        d.data[..data.len()].copy_from_slice(data);
        s.custom = Some(d);
        true
    })
}

fn schedule_beacon(l: &mut Link) {
    w(radio::FREQUENCY, CHANNELS[l.unmapped as usize] as u32);
    w(radio::PACKETPTR, addr_of!(BEACON) as u32);
    w(radio::SHORTS, radio::READY_START | radio::END_DISABLE);
    w(radio::INTENSET, radio::INT_END);
    l.phase = Phase::TxWait;
    radio::schedule(radio::TASKS_TXEN, l.t.wrapping_sub(radio::RAMP_TX));
}

pub(crate) fn on_timer() {
    let l = link();
    match l.phase {
        Phase::TxWait => l.phase = Phase::Tx,
        Phase::RxWait => {
            l.phase = Phase::Rx;
            radio::alarm(l.t.wrapping_add(RX_OFF));
        }
        Phase::Rx => {
            radio::radio_off();
            end_of_frame(l);
        }
        Phase::Tx => {}
    }
}

fn end_of_frame(l: &mut Link) {
    for s in 1..SLOTS {
        let slot = &mut l.slots[s];
        if slot.used && l.frame.wrapping_sub(slot.last_frame) > SLOT_TIMEOUT {
            slot.used = false;
            crate::gamepad::link_drop(s as u8);
            let _ = EVENTS.try_send(Event::LinkDrop { slot: s as u8 });
        }
    }
    l.t = l.t.wrapping_add(FRAME_US);
    l.ts += FRAME_US as u64;
    // Never fire late: a beacon off the 2000 us grid would desync every controller.
    // Skip whole frames instead (the controllers keep hopping through missed beacons).
    while (l.t.wrapping_sub(radio::RAMP_TX).wrapping_sub(radio::now()) as i32) < MIN_LEAD {
        l.stats.late += 1;
        l.t = l.t.wrapping_add(FRAME_US);
        l.ts += FRAME_US as u64;
        l.unmapped = (l.unmapped + l.hop) % 37;
    }
    next_beacon(l);
    schedule_beacon(l);
}

pub(crate) fn on_radio() {
    let l = link();
    if r(radio::EVENTS_ADDRESS) != 0 {
        w(radio::EVENTS_ADDRESS, 0);
        // END_START re-latches PACKETPTR at END: point it at the next buffer now.
        w(radio::PACKETPTR, rx_ptr((l.cur + 1) % NBUF));
    }
    if r(radio::EVENTS_END) == 0 {
        return;
    }
    w(radio::EVENTS_END, 0);
    match l.phase {
        Phase::Tx => {
            // Beacon sent (radio disabling via END_DISABLE): open the uplink window.
            w(radio::PACKETPTR, rx_ptr(l.cur));
            w(radio::SHORTS, radio::READY_START | SHORT_END_START | radio::ADDRESS_RSSISTART);
            w(radio::INTENSET, radio::INT_ADDRESS | radio::INT_END);
            l.phase = Phase::RxWait;
            radio::schedule(radio::TASKS_RXEN, l.t.wrapping_add(RX_ON - radio::RAMP_RX));
        }
        Phase::Rx | Phase::RxWait => {
            let buf = l.cur;
            l.cur = (l.cur + 1) % NBUF;
            if r(radio::CRCSTATUS) & 1 == 0 {
                l.stats.rx_crc_bad += 1;
                return;
            }
            l.stats.rx_crc_ok += 1;
            let rxmatch = r(radio::RXMATCH);
            let raw = unsafe { &*addr_of!(RX[buf]) };
            if !(1..=SLOTS as u32).contains(&rxmatch) || raw[0] != S0_PULSAR || raw[1] <= 4 {
                return;
            }
            let slot = (rxmatch - 1) as u8;
            // Like syncboss, ack on a good CRC in the slot, before decryption.
            l.ack |= 1 << slot;
            let job = Job { buf: buf as u8, slot, rssi: r(radio::RSSISAMPLE) as u8, ts: l.ts };
            if l.jobs.push_back(job).is_err() {
                l.stats.mic_bad += 1;
                return;
            }
            if !l.ccm_busy {
                ccm_start(l);
            }
        }
        Phase::TxWait => {}
    }
}

// ---- uplink decryption: the CCM runs at radio pace (~4 us/byte), so it is queued and
// completed in the CCM interrupt rather than waited for.

#[derive(Clone, Copy)]
struct Job {
    buf: u8,
    slot: u8,
    rssi: u8,
    /// Beacon timestamp of the frame the packet arrived in (the IV).
    ts: u64,
}

fn ccm_start(l: &mut Link) {
    let Some(job) = l.jobs.pop_front() else {
        l.ccm_busy = false;
        return;
    };
    unsafe {
        let cnf = &mut *addr_of_mut!(CNF);
        cnf.0[16..25].fill(0); // packet counter 0, direction 0
        cnf.0[25..31].copy_from_slice(&job.ts.to_le_bytes()[..6]);
        cnf.0[31..33].copy_from_slice(&l.session.to_le_bytes());
    }
    poke(CCM_INPTR, rx_ptr(job.buf as usize));
    poke(CCM_OUTPTR, addr_of!(PLAIN) as u32);
    poke(CCM_EVENTS_ENDCRYPT, 0);
    poke(CCM_EVENTS_ERROR, 0);
    poke(CCM_INTENSET, CCM_INT_ENDCRYPT | CCM_INT_ERROR);
    l.job = job;
    l.ccm_t0 = radio::now();
    l.ccm_busy = true;
    poke(CCM_TASKS_KSGEN, 1);
}

#[no_mangle]
unsafe extern "C" fn AAR_CCM() {
    let l = link();
    let done = peek(CCM_EVENTS_ENDCRYPT) != 0;
    if !done && peek(CCM_EVENTS_ERROR) == 0 {
        return;
    }
    poke(CCM_EVENTS_ENDCRYPT, 0);
    poke(CCM_EVENTS_ERROR, 0);
    let dt = radio::now().wrapping_sub(l.ccm_t0);
    if dt > l.stats.ccm_max_us {
        l.stats.ccm_max_us = dt;
    }
    let mic_ok = done && peek(CCM_MICSTATUS) & 1 != 0;
    let job = l.job;
    uplink(l, job, mic_ok);
    ccm_start(l);
}

fn uplink(l: &mut Link, job: Job, mic_ok: bool) {
    let Job { buf, slot, rssi, .. } = job;
    let raw = unsafe { &*addr_of!(RX[buf as usize]) };
    let len = raw[1] as usize;
    if !mic_ok {
        l.stats.mic_bad += 1;
        let mut data = heapless::Vec::new();
        let _ = data.extend_from_slice(&raw[..(3 + len).min(radio::BUF_LEN)]);
        let _ = EVENTS.try_send(Event::LinkData { slot, rssi, mic_ok: false, data });
        return;
    }
    l.stats.mic_ok += 1;
    let plain = unsafe { &*addr_of!(PLAIN) };
    let p = &plain[3..3 + len - 4];
    if p.is_empty() || p[0] != slot {
        return;
    }
    if slot == 0 {
        // Join request: [0][1 | has_extra << 3][device id 8][type u32].
        if p.len() >= 14 && p[1] & 7 == 1 && l.join.is_none() {
            let mut id = [0u8; 8];
            id.copy_from_slice(&p[2..10]);
            let dev_type = u32::from_le_bytes([p[10], p[11], p[12], p[13]]);
            if let Some(s) = assign_slot(l, &id, dev_type & 0x1_0000 != 0) {
                l.join = Some((id, s));
                l.stats.joins += 1;
                crate::gamepad::link_join(s, dev_type);
                crate::gamepad::power_reset(s, l.frame);
                if crate::settings::get().connect_buzz {
                    l.slots[s as usize].buzz_until = l.frame.wrapping_add(CONNECT_BUZZ_FRAMES);
                }
                let _ = EVENTS.try_send(Event::LinkJoin { slot: s, id, dev_type });
            }
        }
        return;
    }
    let s = &mut l.slots[slot as usize];
    if !s.used {
        return;
    }
    s.last_frame = l.frame;
    // The device echoes the seq of the last request it took in its TL flags.
    if let Some(d) = s.pending {
        if p.len() >= 3 && p[2] & 0x0F == d.seq {
            if let Some(a) = d.haptic {
                s.haptic_done = a;
            }
            s.pending = None;
            l.stats.dl_ok += 1;
        }
    }
    // [slot][tl id][0x40 | seq][log item 4][notification entries...]
    if p.len() > 7 && p[2] & TL_NOTIFICATION != 0 {
        crate::gamepad::link_notifications(slot, &p[7..]);
        match crate::gamepad::power_policy(slot, l.frame) {
            crate::gamepad::PowerAction::ArmAll => {
                for s in 1..SLOTS {
                    if l.slots[s].used {
                        crate::gamepad::arm_power_off(s as u8, l.frame);
                        l.slots[s].buzz_until = l.frame.wrapping_add(BUZZ_FRAMES);
                    }
                }
            }
            crate::gamepad::PowerAction::OffThis => queue_power_off(&mut l.slots[slot as usize]),
            crate::gamepad::PowerAction::LowBattery => {
                l.slots[slot as usize].buzz_until = l.frame.wrapping_add(LOW_BATT_BUZZ_FRAMES);
            }
            crate::gamepad::PowerAction::DeadBattery => {
                for s in 1..SLOTS {
                    if s != slot as usize && l.slots[s].used {
                        l.slots[s].buzz_until = l.frame.wrapping_add(LOW_BATT_BUZZ_FRAMES);
                    }
                }
            }
            crate::gamepad::PowerAction::NextMode => {
                l.slots[slot as usize].buzz_until = l.frame.wrapping_add(BUZZ_FRAMES);
                crate::ui::MODE_REQUEST.signal(());
            }
            crate::gamepad::PowerAction::None => {}
        }
    }
    let mut data = heapless::Vec::new();
    let _ = data.extend_from_slice(p);
    let _ = EVENTS.try_send(Event::LinkData { slot, rssi, mic_ok: true, data });
}

/// Reuse the device's slot if it is rejoining, else take the first free one (1..4).
fn assign_slot(l: &mut Link, id: &[u8; 8], right: bool) -> Option<u8> {
    let s = (1..SLOTS)
        .find(|&s| l.slots[s].used && l.slots[s].id == *id)
        .or_else(|| (1..SLOTS).find(|&s| !l.slots[s].used))?;
    // A (re)joining device starts with fresh TL state and its motor off.
    l.slots[s] = Slot { used: true, id: *id, last_frame: l.frame, right, ..EMPTY_SLOT };
    Some(s as u8)
}
