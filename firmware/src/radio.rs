//! nRF52840 RADIO engine for the Pulsar PHY.
//!
//! Registers are poked directly (mirroring the headset's `pulsar_phy.c`). Timing
//! uses TIMER0 as a free-running 1 MHz clock:
//!   - fixed PPI ch26/27 capture RADIO ADDRESS/END into CC[1]/CC[2],
//!   - PPI ch0 fires a RADIO task (TXEN/RXEN) on COMPARE[3] for scheduled starts,
//!   - COMPARE[3] also interrupts, which drives RX-window timeouts.
//!
//! Modes:
//!   - Sniff: continuous RX, every packet goes to the host.
//!   - Raw TX: send one packet (optionally then RX).
//!   - DM host: the controller SPL's device-management link (pairing, etc.),
//!     replicating syncboss `pulsar_dm_host.c` timing.
//!   - Link host: the main Pulsar link to paired controllers (`link.rs`).

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

pub const RADIO_BASE: u32 = 0x4000_1000;
const TIMER0_BASE: u32 = 0x4000_8000;
const PPI: u32 = 0x4001_F000;

// RADIO register offsets.
pub(crate) const TASKS_TXEN: u32 = 0x000;
pub(crate) const TASKS_RXEN: u32 = 0x004;
pub(crate) const TASKS_DISABLE: u32 = 0x010;
const EVENTS_READY: u32 = 0x100;
pub(crate) const EVENTS_ADDRESS: u32 = 0x104;
pub(crate) const EVENTS_END: u32 = 0x10C;
pub(crate) const EVENTS_DISABLED: u32 = 0x110;
pub(crate) const SHORTS: u32 = 0x200;
pub(crate) const INTENSET: u32 = 0x304;
const INTENCLR: u32 = 0x308;
pub(crate) const CRCSTATUS: u32 = 0x400;
pub(crate) const RXMATCH: u32 = 0x408;
pub(crate) const PACKETPTR: u32 = 0x504;
pub(crate) const FREQUENCY: u32 = 0x508;
const TXPOWER: u32 = 0x50C;
pub(crate) const MODE: u32 = 0x510;
pub(crate) const PCNF0: u32 = 0x514;
pub(crate) const PCNF1: u32 = 0x518;
pub(crate) const BASE0: u32 = 0x51C;
pub(crate) const BASE1: u32 = 0x520;
pub(crate) const PREFIX0: u32 = 0x524;
pub(crate) const PREFIX1: u32 = 0x528;
pub(crate) const TXADDRESS: u32 = 0x52C;
pub(crate) const RXADDRESSES: u32 = 0x530;
const CRCCNF: u32 = 0x534;
const CRCPOLY: u32 = 0x538;
const CRCINIT: u32 = 0x53C;
pub(crate) const RSSISAMPLE: u32 = 0x548;
const STATE: u32 = 0x550;
const MODECNF0: u32 = 0x650;
const POWER: u32 = 0xFFC;

// SHORTS bits.
pub(crate) const READY_START: u32 = 1 << 0;
pub(crate) const END_DISABLE: u32 = 1 << 1;
const DISABLED_RXEN: u32 = 1 << 3;
pub(crate) const ADDRESS_RSSISTART: u32 = 1 << 4;
pub(crate) const DISABLED_RSSISTOP: u32 = 1 << 8;

// RADIO INTEN bits.
pub(crate) const INT_ADDRESS: u32 = 1 << 1;
pub(crate) const INT_END: u32 = 1 << 3;

// TIMER0 registers.
const T_CAPTURE0: u32 = TIMER0_BASE + 0x040;
const T_COMPARE3: u32 = TIMER0_BASE + 0x14C;
const T_INTENSET: u32 = TIMER0_BASE + 0x304;
const T_INTENCLR: u32 = TIMER0_BASE + 0x308;
const T_CC0: u32 = TIMER0_BASE + 0x540;
pub(crate) const T_CC1: u32 = TIMER0_BASE + 0x544;
pub(crate) const T_CC2: u32 = TIMER0_BASE + 0x548;
const T_CC3: u32 = TIMER0_BASE + 0x54C;
const T_INT_COMPARE3: u32 = 1 << 19;

const PPI_CHENSET: u32 = PPI + 0x504;
const PPI_CHENCLR: u32 = PPI + 0x508;
const PPI_CH0_EEP: u32 = PPI + 0x510;
const PPI_CH0_TEP: u32 = PPI + 0x514;

/// Pulsar DM advertising: 2402 MHz, AA:FACEB00C. DM link: 2426 MHz, AA:<id0>.
pub const ADV_BASE: u32 = 0xFACE_B00C;
pub const DM_PREFIX: u32 = 0xAA;
const ADV_FREQ: u32 = 2;
const DM_FREQ: u32 = 26;
pub(crate) const RAMP_TX: u32 = 40;
pub(crate) const RAMP_RX: u32 = 44;
/// Consecutive missed responses before the link is declared lost (syncboss: 666).
const DM_MAX_MISSES: u32 = 666;

pub const BUF_LEN: usize = 260;

#[inline(always)]
pub(crate) fn w(off: u32, val: u32) {
    unsafe { write_volatile((RADIO_BASE + off) as *mut u32, val) }
}
#[inline(always)]
pub(crate) fn r(off: u32) -> u32 {
    unsafe { read_volatile((RADIO_BASE + off) as *const u32) }
}
#[inline(always)]
pub fn poke(addr: u32, val: u32) {
    unsafe { write_volatile(addr as *mut u32, val) }
}
#[inline(always)]
pub fn peek(addr: u32) -> u32 {
    unsafe { read_volatile(addr as *const u32) }
}

pub enum Event {
    Rx { ts: u32, crc_ok: bool, rxmatch: u8, rssi: u8, data: heapless::Vec<u8, BUF_LEN> },
    TxDone { ts: u32 },
    DmLink { connected: bool, id0: u32, id1: u32 },
    DmResp { hdr: u8, seq: u8, data: heapless::Vec<u8, BUF_LEN> },
    LinkJoin { slot: u8, id: [u8; 8], dev_type: u32 },
    LinkDrop { slot: u8 },
    /// Decrypted uplink payload (starting at its slot byte); `mic_ok` false = raw ciphertext.
    LinkData { slot: u8, rssi: u8, mic_ok: bool, data: heapless::Vec<u8, BUF_LEN> },
    /// Pairing progress (ui::PairState as u8).
    PairStatus(u8),
}

pub static EVENTS: Channel<CriticalSectionRawMutex, Event, 24> = Channel::new();

/// DM link state / responses for firmware-driven pairing (the host gets the same via EVENTS).
pub static DM_LINK: embassy_sync::signal::Signal<CriticalSectionRawMutex, bool> = embassy_sync::signal::Signal::new();
pub static DM_RESP: embassy_sync::signal::Signal<CriticalSectionRawMutex, u8> = embassy_sync::signal::Signal::new();

/// Where the LENGTH byte sits in the RAM buffer, and how many bytes precede the payload.
/// Defaults match PCNF0 = 0x00100108: [S0][LEN][S1][payload].
pub static LEN_OFF: AtomicU8 = AtomicU8::new(1);
pub static HDR_LEN: AtomicU8 = AtomicU8::new(3);
pub static FORWARD_BAD_CRC: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Mode {
    Idle,
    Sniff,
    RawTx { rx_after: bool },
    Dm(DmPhase),
    Link,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DmPhase {
    Scan,
    TxWait,
    Tx,
    RxWait,
    Rx,
}

struct Dm {
    want_id0: u32,
    want_id1: u32,
    any: bool,
    id0: u32,
    id1: u32,
    seq: u8,
    pending: bool,
    cmd: [u8; BUF_LEN],
    cmd_len: usize,
    misses: u32,
    got_addr: bool,
    deadline: u32,
    pub tx_count: u32,
    pub rx_count: u32,
}

pub(crate) struct Engine {
    pub(crate) mode: Mode,
    cur: usize,
    dm: Dm,
}

static mut ENG: Engine = Engine {
    mode: Mode::Idle,
    cur: 0,
    dm: Dm {
        want_id0: 0,
        want_id1: 0,
        any: false,
        id0: 0,
        id1: 0,
        seq: 0,
        pending: false,
        cmd: [0; BUF_LEN],
        cmd_len: 0,
        misses: 0,
        got_addr: false,
        deadline: 0,
        tx_count: 0,
        rx_count: 0,
    },
};
static mut RX_BUFS: [[u8; BUF_LEN]; 2] = [[0; BUF_LEN]; 2];
static mut TX_BUF: [u8; BUF_LEN] = [0; BUF_LEN];

pub(crate) fn eng() -> &'static mut Engine {
    unsafe { &mut *addr_of_mut!(ENG) }
}
pub(crate) fn rx_buf_ptr(i: usize) -> u32 {
    unsafe { addr_of!(RX_BUFS[i]) as u32 }
}
fn tx_buf() -> &'static mut [u8; BUF_LEN] {
    unsafe { &mut *addr_of_mut!(TX_BUF) }
}

pub fn init() {
    poke(TIMER0_BASE + 0x504, 0); // MODE = timer
    poke(TIMER0_BASE + 0x508, 3); // BITMODE = 32
    poke(TIMER0_BASE + 0x510, 4); // PRESCALER = 16 MHz / 16 = 1 MHz
    poke(TIMER0_BASE + 0x00C, 1); // TASKS_CLEAR
    poke(TIMER0_BASE + 0x000, 1); // TASKS_START
    poke(PPI_CHENSET, (1 << 26) | (1 << 27)); // ADDRESS -> CC[1], END -> CC[2]
    poke(PPI_CH0_EEP, T_COMPARE3);

    w(POWER, 0);
    w(POWER, 1);
    // Pulsar PHY defaults (syncboss pulsar_phy.c init).
    w(MODECNF0, 1);
    w(MODE, 1); // Nrf_2Mbit
    w(PCNF0, 0x0010_0108);
    w(PCNF1, 0x0104_0000 | 255);
    w(CRCCNF, 3);
    w(CRCPOLY, 0x0110_8421);
    w(CRCINIT, 0x00FF_FFFF);
    w(TXPOWER, 8); // +8 dBm
    unsafe {
        use embassy_nrf::interrupt::{InterruptExt, Priority};
        for irq in [embassy_nrf::interrupt::RADIO, embassy_nrf::interrupt::TIMER0] {
            irq.set_priority(Priority::P0);
            irq.unpend();
            irq.enable();
        }
    }
}

pub fn now() -> u32 {
    poke(T_CAPTURE0, 1);
    peek(T_CC0)
}

/// Fire RADIO task `task` at absolute TIMER0 time `at`, and interrupt then.
pub(crate) fn schedule(task: u32, at: u32) {
    poke(PPI_CHENCLR, 1);
    poke(T_COMPARE3, 0);
    poke(PPI_CH0_TEP, RADIO_BASE + task);
    poke(T_CC3, at);
    poke(PPI_CHENSET, 1);
    poke(T_INTENSET, T_INT_COMPARE3);
    // Already in the past (ISR ran late)? Trigger by hand.
    if now().wrapping_sub(at) < 0x8000_0000 && peek(T_COMPARE3) == 0 {
        poke(PPI_CHENCLR, 1);
        w(task, 1);
        poke(T_CC3, now().wrapping_add(2));
    }
}

/// Interrupt (without a radio task) at absolute time `at`.
pub(crate) fn alarm(at: u32) {
    poke(PPI_CHENCLR, 1);
    poke(T_COMPARE3, 0);
    poke(T_CC3, at);
    poke(T_INTENSET, T_INT_COMPARE3);
}

pub(crate) fn cancel_timer() {
    poke(PPI_CHENCLR, 1);
    poke(T_INTENCLR, T_INT_COMPARE3);
    poke(T_COMPARE3, 0);
}

pub(crate) fn radio_off() {
    w(INTENCLR, 0xFFFF_FFFF);
    w(SHORTS, 0);
    w(TASKS_DISABLE, 1);
    while r(STATE) != 0 {}
    w(EVENTS_DISABLED, 0);
    w(EVENTS_ADDRESS, 0);
    w(EVENTS_END, 0);
    w(EVENTS_READY, 0);
}

fn set_addr(base: u32, prefix: u32, freq: u32) {
    w(BASE0, base);
    w(BASE1, base);
    w(PREFIX0, prefix);
    w(PREFIX1, 0);
    w(TXADDRESS, 0);
    w(RXADDRESSES, 1);
    w(FREQUENCY, freq);
}

fn continuous_rx() {
    eng().cur = 0;
    w(PACKETPTR, rx_buf_ptr(0));
    w(SHORTS, READY_START | END_DISABLE | DISABLED_RXEN | ADDRESS_RSSISTART | DISABLED_RSSISTOP);
    w(INTENSET, INT_ADDRESS | INT_END);
    w(TASKS_RXEN, 1);
}

// ---- thread-mode API (each call runs in a critical section vs. the ISRs) ----

pub fn stop() {
    critical_section::with(|_| {
        cancel_timer();
        radio_off();
        eng().mode = Mode::Idle;
    })
}

/// Stop whatever is running and hand the radio to the link engine.
pub(crate) fn take_for_link() {
    cancel_timer();
    radio_off();
    eng().mode = Mode::Link;
}

pub fn rx_start() {
    critical_section::with(|_| {
        cancel_timer();
        radio_off();
        eng().mode = Mode::Sniff;
        continuous_rx();
    })
}

/// Transmit one raw RAM-layout packet, optionally falling straight into RX.
pub fn tx(data: &[u8], rx_after: bool) {
    critical_section::with(|_| {
        cancel_timer();
        radio_off();
        let n = data.len().min(BUF_LEN);
        tx_buf()[..n].copy_from_slice(&data[..n]);
        let e = eng();
        e.mode = Mode::RawTx { rx_after };
        e.cur = 0;
        w(PACKETPTR, addr_of!(TX_BUF) as u32);
        w(SHORTS, READY_START | END_DISABLE | ADDRESS_RSSISTART | DISABLED_RSSISTOP);
        w(INTENSET, INT_ADDRESS | INT_END);
        w(TASKS_TXEN, 1);
    })
}

/// Start the DM host: scan advertisements and connect to the matching controller
/// (or the first one seen if `any`).
pub fn dm_start(id0: u32, id1: u32, any: bool) {
    critical_section::with(|_| {
        cancel_timer();
        radio_off();
        let e = eng();
        e.dm.want_id0 = id0;
        e.dm.want_id1 = id1;
        e.dm.any = any;
        e.dm.pending = false;
        e.dm.tx_count = 0;
        e.dm.rx_count = 0;
        dm_scan(e);
    })
}

/// Queue a DM command; the response arrives as `Event::DmResp`. False if busy/not connected.
pub fn dm_send(hdr: u8, payload: &[u8]) -> bool {
    critical_section::with(|_| {
        let e = eng();
        let connected = matches!(e.mode, Mode::Dm(p) if p != DmPhase::Scan);
        if !connected || e.dm.pending || payload.len() > 250 {
            return false;
        }
        e.dm.seq = e.dm.seq.wrapping_add(1);
        e.dm.cmd[0] = hdr;
        e.dm.cmd[1..1 + payload.len()].copy_from_slice(payload);
        e.dm.cmd_len = 1 + payload.len();
        e.dm.pending = true;
        true
    })
}

pub fn dm_stats() -> (u32, u32, u32, u8) {
    critical_section::with(|_| {
        let d = &eng().dm;
        (d.tx_count, d.rx_count, d.misses, d.seq)
    })
}

// ---- DM host state machine (interrupt context) ----

fn dm_scan(e: &mut Engine) {
    radio_off();
    w(PCNF0, 8);
    set_addr(ADV_BASE, DM_PREFIX, ADV_FREQ);
    e.mode = Mode::Dm(DmPhase::Scan);
    continuous_rx();
}

/// Build the next host packet ([LEN][hdr][seq][payload]) and schedule it.
fn dm_schedule_tx(e: &mut Engine, at: u32) {
    let tb = tx_buf();
    if e.dm.pending {
        tb[0] = 1 + e.dm.cmd_len as u8;
        tb[1] = e.dm.cmd[0];
        tb[2] = e.dm.seq;
        tb[3..2 + e.dm.cmd_len].copy_from_slice(&e.dm.cmd[1..e.dm.cmd_len]);
    } else {
        tb[0] = 2;
        tb[1] = 0;
        tb[2] = e.dm.seq;
    }
    w(PACKETPTR, addr_of!(TX_BUF) as u32);
    w(SHORTS, READY_START | END_DISABLE);
    w(INTENSET, INT_END | INT_ADDRESS);
    e.mode = Mode::Dm(DmPhase::TxWait);
    schedule(TASKS_TXEN, at.wrapping_sub(RAMP_TX));
}

fn dm_on_end(e: &mut Engine, phase: DmPhase, t_end: u32, crc_ok: bool, buf: &[u8; BUF_LEN]) {
    match phase {
        DmPhase::Scan => {
            if !crc_ok {
                return;
            }
            forward_rx(t_end, true, buf, 0, 1);
            // Advertisement payload: [ver][type u32][id0 u32][id1 u32]... (ver 2) or
            // [ver][?][type u32][id0][id1] (ver 1).
            let p = &buf[1..];
            let (id0, id1) = match p[0] {
                2 => (le32(&p[5..]), le32(&p[9..])),
                1 => (le32(&p[6..]), le32(&p[10..])),
                _ => return,
            };
            if !(e.dm.any || (id0 == e.dm.want_id0 && id1 == e.dm.want_id1)) {
                return;
            }
            radio_off();
            e.dm.id0 = id0;
            e.dm.id1 = id1;
            e.dm.seq = 0;
            e.dm.pending = false;
            e.dm.misses = 0;
            set_addr(id0, DM_PREFIX, DM_FREQ);
            DM_LINK.signal(true);
            let _ = EVENTS.try_send(Event::DmLink { connected: true, id0, id1 });
            // The controller listens 900..2000 us after its advertisement; syncboss TXes at +1000.
            dm_schedule_tx(e, t_end.wrapping_add(1000));
        }
        DmPhase::Tx => {
            e.dm.tx_count += 1;
            w(PACKETPTR, rx_buf_ptr(0));
            w(SHORTS, READY_START | END_DISABLE | ADDRESS_RSSISTART | DISABLED_RSSISTOP);
            e.dm.got_addr = false;
            e.dm.deadline = t_end.wrapping_add(2200);
            e.mode = Mode::Dm(DmPhase::RxWait);
            schedule(TASKS_RXEN, t_end.wrapping_add(700 - RAMP_RX));
        }
        DmPhase::Rx => {
            e.dm.rx_count += 1;
            e.dm.misses = 0;
            if crc_ok {
                let len = buf[0] as usize;
                if len >= 2 {
                    let (hdr, seq) = (buf[1], buf[2]);
                    if e.dm.pending && seq == e.dm.seq && hdr & 1 == 0 {
                        e.dm.pending = false;
                        let mut data = heapless::Vec::new();
                        let _ = data.extend_from_slice(&buf[3..1 + len]);
                        DM_RESP.signal(hdr);
                        let _ = EVENTS.try_send(Event::DmResp { hdr, seq, data });
                    }
                }
            }
            dm_schedule_tx(e, t_end.wrapping_add(1000));
        }
        _ => {}
    }
}

fn dm_on_timer(e: &mut Engine, phase: DmPhase) {
    match phase {
        DmPhase::TxWait => e.mode = Mode::Dm(DmPhase::Tx),
        DmPhase::RxWait => {
            e.mode = Mode::Dm(DmPhase::Rx);
            alarm(e.dm.deadline);
        }
        DmPhase::Rx if !e.dm.got_addr => {
            radio_off();
            e.dm.misses += 1;
            if e.dm.misses > DM_MAX_MISSES {
                DM_LINK.signal(false);
                let _ = EVENTS.try_send(Event::DmLink { connected: false, id0: e.dm.id0, id1: e.dm.id1 });
                dm_scan(e);
            } else {
                dm_schedule_tx(e, now().wrapping_add(1000));
            }
        }
        _ => {}
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn forward_rx(ts: u32, crc_ok: bool, buf: &[u8; BUF_LEN], lo: usize, hdr: usize) {
    let n = (hdr + buf[lo] as usize).min(BUF_LEN);
    let mut data = heapless::Vec::new();
    let _ = data.extend_from_slice(&buf[..n]);
    let _ = EVENTS.try_send(Event::Rx { ts, crc_ok, rxmatch: r(RXMATCH) as u8, rssi: r(RSSISAMPLE) as u8, data });
}

#[no_mangle]
unsafe extern "C" fn TIMER0() {
    if peek(T_COMPARE3) == 0 {
        return;
    }
    poke(T_COMPARE3, 0);
    poke(PPI_CHENCLR, 1);
    poke(T_INTENCLR, T_INT_COMPARE3);
    let e = eng();
    match e.mode {
        Mode::Dm(phase) => dm_on_timer(e, phase),
        Mode::Link => crate::link::on_timer(),
        _ => {}
    }
}

#[no_mangle]
unsafe extern "C" fn RADIO() {
    let e = eng();
    if e.mode == Mode::Link {
        crate::link::on_radio();
        return;
    }
    if r(EVENTS_ADDRESS) != 0 {
        w(EVENTS_ADDRESS, 0);
        match e.mode {
            // The radio latched PACKETPTR at START; queue the other buffer for the next packet.
            Mode::Sniff | Mode::Dm(DmPhase::Scan) => w(PACKETPTR, rx_buf_ptr(e.cur ^ 1)),
            Mode::RawTx { .. } => w(PACKETPTR, rx_buf_ptr(0)),
            Mode::Dm(DmPhase::Rx) | Mode::Dm(DmPhase::RxWait) => {
                e.dm.got_addr = true;
                e.mode = Mode::Dm(DmPhase::Rx);
            }
            _ => {}
        }
    }
    if r(EVENTS_END) == 0 {
        return;
    }
    w(EVENTS_END, 0);
    let t_end = peek(T_CC2);
    let crc_ok = r(CRCSTATUS) & 1 != 0;
    match e.mode {
        Mode::RawTx { rx_after } => {
            if rx_after {
                e.mode = Mode::Sniff;
                w(SHORTS, READY_START | END_DISABLE | DISABLED_RXEN | ADDRESS_RSSISTART | DISABLED_RSSISTOP);
                w(TASKS_RXEN, 1);
            } else {
                e.mode = Mode::Idle;
                w(SHORTS, 0);
            }
            let _ = EVENTS.try_send(Event::TxDone { ts: t_end });
        }
        Mode::Sniff => {
            let buf = &*addr_of!(RX_BUFS[e.cur]);
            e.cur ^= 1;
            if crc_ok || FORWARD_BAD_CRC.load(Ordering::Relaxed) {
                let lo = LEN_OFF.load(Ordering::Relaxed) as usize;
                let hdr = HDR_LEN.load(Ordering::Relaxed) as usize;
                forward_rx(peek(T_CC1), crc_ok, buf, lo, hdr);
            }
        }
        Mode::Dm(phase) => {
            let idx = if phase == DmPhase::Scan { e.cur } else { 0 };
            if phase == DmPhase::Scan {
                e.cur ^= 1;
            }
            let buf = &*addr_of!(RX_BUFS[idx]);
            dm_on_end(e, phase, t_end, crc_ok, buf);
        }
        Mode::Idle | Mode::Link => {}
    }
}
