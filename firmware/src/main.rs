#![no_std]
#![no_main]

mod cobs;
mod ds4;
mod gamepad;
mod link;
mod pairing;
mod radio;
mod selfupdate;
mod settings;
mod ui;
mod wdt;
mod xinput;

use core::panic::PanicInfo;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::Driver;
use embassy_nrf::gpio::{Input, Level, Pull};
use embassy_nrf::pwm::{Prescaler, SimpleConfig, SimplePwm};
use embassy_nrf::{bind_interrupts, peripherals, usb};
use embassy_time::{Duration, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, Receiver, Sender, State};
use embassy_usb::class::hid::{self, HidReader, HidReaderWriter, HidWriter};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, UsbDevice};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBD => usb::InterruptHandler<peripherals::USBD>;
    CLOCK_POWER => usb::vbus_detect::InterruptHandler;
});

type UsbDriver = Driver<'static, HardwareVbusDetect>;

const VERSION: &[u8] = b"questpad 1.3.0";

// Host -> dongle commands.
const CMD_POKE: u8 = 0x01; // addr u32, val u32
const CMD_RX: u8 = 0x02;
const CMD_STOP: u8 = 0x03;
const CMD_TX: u8 = 0x04; // flags u8 (bit0: rx after), data...
const CMD_PING: u8 = 0x05;
const CMD_LAYOUT: u8 = 0x06; // len_off u8, hdr_len u8, forward_bad_crc u8
const CMD_PEEK: u8 = 0x07; // addr u32
const CMD_DM_START: u8 = 0x10; // id0 u32, id1 u32, any u8
const CMD_DM_SEND: u8 = 0x11; // hdr u8, payload...
const CMD_DM_STATS: u8 = 0x12;
const CMD_LINK_START: u8 = 0x20; // network_id u32, link_key [16], session u16
const CMD_LINK_STATS: u8 = 0x21;
const CMD_PAIR_STORE: u8 = 0x22; // network_id u32, link_key [16]: save to flash and (re)start the link
const CMD_PAIR_GET: u8 = 0x23;
const CMD_LINK_RESUME: u8 = 0x24; // (re)start the link from the stored pairing
const CMD_RUMBLE: u8 = 0x25; // left u8, right u8 amplitude
const CMD_LINK_REQ: u8 = 0x26; // slot u8, reg u8, read u8, data... (TL request; replies arrive as EV_LINK_DATA)
const CMD_LINK_SLOTS: u8 = 0x27;
const CMD_POWER_OFF: u8 = 0x28; // power off all controllers
const CMD_SETTINGS_GET: u8 = 0x29;
const CMD_SETTINGS_SET: u8 = 0x2A; // settings [settings::LEN], persist u8 (a mode change reboots)
const CMD_PAIR_START: u8 = 0x2B; // start pairing (or cancel it)
const CMD_PAIR_EXPORT: u8 = 0x2C;
const CMD_PAIR_FORGET: u8 = 0x2D;
const CMD_STATUS: u8 = 0x2E;
const CMD_PAD_SET: u8 = 0x30; // 13-byte gamepad report
const CMD_FW_BEGIN: u8 = 0x70; // len u32
const CMD_FW_DATA: u8 = 0x71; // off u32, data...
const CMD_FW_COMMIT: u8 = 0x72; // len u32, crc32 u32
const CMD_DFU: u8 = 0x7F;

// Dongle -> host events.
const EV_RX: u8 = 0x81; // ts u32, crc_ok u8, rxmatch u8, rssi u8, data...
const EV_TXDONE: u8 = 0x82; // ts u32
const EV_PONG: u8 = 0x84; // now u32, version...
const EV_PEEK: u8 = 0x87; // addr u32, val u32
const EV_DM_LINK: u8 = 0x90; // connected u8, id0 u32, id1 u32
const EV_DM_RESP: u8 = 0x91; // hdr u8, seq u8, data...
const EV_DM_STATS: u8 = 0x92; // tx u32, rx u32, misses u32, seq u8
const EV_DM_SENT: u8 = 0x93; // accepted u8
const EV_LINK_JOIN: u8 = 0xA0; // slot u8, device_id [8], type u32
const EV_LINK_DATA: u8 = 0xA1; // slot u8, rssi u8, mic_ok u8, payload (plaintext from the slot byte, else raw)
const EV_LINK_DROP: u8 = 0xA2; // slot u8
const EV_LINK_STATS: u8 = 0xA3; // 10 x u32 (link::Stats order)
const EV_PAIR: u8 = 0xA4; // stored u8, network_id u32, link running u8
const EV_LINK_SLOTS: u8 = 0xA5; // n x (slot u8, right u8, device_id [8])
const EV_SETTINGS: u8 = 0xA6; // settings [settings::LEN]
const EV_PAIR_STATUS: u8 = 0xA7; // ui::PairState u8
const EV_PAIR_EXPORT: u8 = 0xA8; // stored u8, network_id u32, link_key [16]
const EV_STATUS: u8 = 0xA9; // mode u8, paired u8, pair_state u8, n u8, n x (slot u8, right u8, battery u8 (255 = unknown)), n x status flags u8 (1.2.0+)
const EV_FW: u8 = 0xF0; // cmd u8, ok u8

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}

#[embassy_executor::task]
async fn hid_task(mut writer: HidWriter<'static, UsbDriver, { gamepad::REPORT_LEN }>) -> ! {
    writer.ready().await;
    let _ = writer.write(&gamepad::NEUTRAL).await;
    loop {
        let report = match select(gamepad::PAD.wait(), gamepad::OVERRIDE.wait()).await {
            Either::First(pad) => gamepad::hid_report(&pad),
            Either::Second(raw) => raw,
        };
        let _ = writer.write(&report).await;
    }
}

/// USB bus events: when the PC suspends (sleeps), power the controllers off.
struct UsbEvents;

static SUSPENDED: embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

impl embassy_usb::Handler for UsbEvents {
    fn suspended(&mut self, suspended: bool) {
        if suspended {
            SUSPENDED.signal(());
        }
    }
}

#[embassy_executor::task]
async fn suspend_task() -> ! {
    loop {
        SUSPENDED.wait().await;
        if settings::get().off_on_suspend {
            link::power_off_all();
        }
    }
}

/// DS4 mode: stream input reports (at least every 8 ms, like a real pad; usually 2 ms from the IMU).
#[embassy_executor::task]
async fn ds4_task(mut writer: HidWriter<'static, UsbDriver, { ds4::IN_LEN }>) -> ! {
    writer.ready().await;
    let mut pad = gamepad::Pad::default();
    loop {
        if let Ok(p) = embassy_time::with_timeout(Duration::from_millis(8), gamepad::PAD.wait()).await {
            pad = p;
        }
        let _ = writer.write(&ds4::report(&pad)).await;
    }
}

#[embassy_executor::task]
async fn ds4_out_task(reader: HidReader<'static, UsbDriver, { ds4::OUT_LEN }>) -> ! {
    reader.run(true, &mut ds4::Handler).await
}

#[embassy_executor::task]
async fn xinput_in_task(ep: usb::Endpoint<'static, usb::In>) -> ! {
    xinput::run_in(ep).await
}

#[embassy_executor::task]
async fn xinput_out_task(ep: usb::Endpoint<'static, usb::Out>) -> ! {
    xinput::run_out(ep).await
}

#[embassy_executor::task]
async fn rumble_task(reader: HidReader<'static, UsbDriver, { gamepad::OUT_REPORT_LEN }>) -> ! {
    reader.run(false, &mut gamepad::Rumble).await
}

#[embassy_executor::task]
async fn usb_task(mut dev: UsbDevice<'static, UsbDriver>) -> ! {
    dev.run().await
}

async fn send_frame(tx: &mut Sender<'static, UsbDriver>, raw: &[u8]) -> Result<(), EndpointError> {
    let mut enc = [0u8; 300];
    let n = cobs::encode(raw, &mut enc);
    enc[n] = 0;
    let total = n + 1;
    for chunk in enc[..total].chunks(64) {
        tx.write_packet(chunk).await?;
    }
    if total % 64 == 0 {
        tx.write_packet(&[]).await?;
    }
    Ok(())
}

fn rd_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Host the link from the pairing in flash, if any.
fn start_stored_link() {
    if let Some(p) = pairing::load() {
        link::start(p.network_id, &p.key, link::SESSION);
    }
}

fn enter_dfu() -> ! {
    // Nordic's Open USB DFU bootloader stays in DFU mode when GPREGRET == 0xB1.
    radio::poke(0x4000_051C, 0xB1);
    cortex_m::peripheral::SCB::sys_reset()
}

/// Handle one decoded host command; returns an optional reply frame length written into `out`.
fn handle(cmd: &[u8], out: &mut [u8]) -> usize {
    if cmd.is_empty() {
        return 0;
    }
    let a = &cmd[1..];
    match cmd[0] {
        CMD_POKE if a.len() >= 8 => radio::poke(rd_u32(a), rd_u32(&a[4..])),
        CMD_PEEK if a.len() >= 4 => {
            let addr = rd_u32(a);
            out[0] = EV_PEEK;
            out[1..5].copy_from_slice(&addr.to_le_bytes());
            out[5..9].copy_from_slice(&radio::peek(addr).to_le_bytes());
            return 9;
        }
        CMD_RX => radio::rx_start(),
        CMD_STOP => radio::stop(),
        CMD_TX if !a.is_empty() => radio::tx(&a[1..], a[0] & 1 != 0),
        CMD_LAYOUT if a.len() >= 3 => {
            use core::sync::atomic::Ordering::Relaxed;
            radio::LEN_OFF.store(a[0], Relaxed);
            radio::HDR_LEN.store(a[1], Relaxed);
            radio::FORWARD_BAD_CRC.store(a[2] != 0, Relaxed);
        }
        CMD_PING => {
            out[0] = EV_PONG;
            out[1..5].copy_from_slice(&radio::now().to_le_bytes());
            out[5..5 + VERSION.len()].copy_from_slice(VERSION);
            return 5 + VERSION.len();
        }
        CMD_DM_START if a.len() >= 9 => radio::dm_start(rd_u32(a), rd_u32(&a[4..]), a[8] != 0),
        CMD_DM_SEND if !a.is_empty() => {
            out[0] = EV_DM_SENT;
            out[1] = radio::dm_send(a[0], &a[1..]) as u8;
            return 2;
        }
        CMD_DM_STATS => {
            let (tx, rx, miss, seq) = radio::dm_stats();
            out[0] = EV_DM_STATS;
            out[1..5].copy_from_slice(&tx.to_le_bytes());
            out[5..9].copy_from_slice(&rx.to_le_bytes());
            out[9..13].copy_from_slice(&miss.to_le_bytes());
            out[13] = seq;
            return 14;
        }
        CMD_LINK_START if a.len() >= 22 => {
            let mut key = [0u8; 16];
            key.copy_from_slice(&a[4..20]);
            link::start(rd_u32(a), &key, u16::from_le_bytes([a[20], a[21]]));
        }
        CMD_PAIR_STORE if a.len() >= 20 => {
            let mut key = [0u8; 16];
            key.copy_from_slice(&a[4..20]);
            let p = pairing::Pairing { network_id: rd_u32(a), key };
            radio::stop();
            let ok = pairing::store(&p);
            link::start(p.network_id, &p.key, link::SESSION);
            out[0] = EV_FW;
            out[1] = cmd[0];
            out[2] = ok as u8;
            return 3;
        }
        CMD_PAIR_GET => {
            let p = pairing::load();
            out[0] = EV_PAIR;
            out[1] = p.is_some() as u8;
            out[2..6].copy_from_slice(&p.map_or(0, |p| p.network_id).to_le_bytes());
            out[6] = link::running() as u8;
            return 7;
        }
        CMD_LINK_RESUME => start_stored_link(),
        CMD_LINK_STATS => {
            let s = link::stats();
            out[0] = EV_LINK_STATS;
            let v = [
                s.frames, s.rx_crc_ok, s.rx_crc_bad, s.mic_ok, s.mic_bad, s.joins, s.late, s.ccm_max_us, s.dl_ok,
                s.dl_fail,
            ];
            for (i, x) in v.iter().enumerate() {
                out[1 + 4 * i..5 + 4 * i].copy_from_slice(&x.to_le_bytes());
            }
            return 1 + 4 * v.len();
        }
        CMD_LINK_SLOTS => {
            out[0] = EV_LINK_SLOTS;
            let mut n = 1;
            for (slot, right, id) in link::slots() {
                out[n] = slot;
                out[n + 1] = right as u8;
                out[n + 2..n + 10].copy_from_slice(&id);
                n += 10;
            }
            return n;
        }
        CMD_POWER_OFF => link::power_off_all(),
        CMD_SETTINGS_GET => {
            out[0] = EV_SETTINGS;
            out[1..1 + settings::LEN].copy_from_slice(&settings::get().to_bytes());
            return 1 + settings::LEN;
        }
        CMD_SETTINGS_SET if a.len() >= settings::LEN + 1 => {
            let Some(new) = settings::Settings::from_bytes(a) else { return 0 };
            let ok = if a[settings::LEN] != 0 {
                let mode_changed = new.mode != settings::get().mode;
                let running = link::running();
                radio::stop();
                let ok = settings::store(&new);
                if mode_changed {
                    // Re-enumerate as the new device type.
                    cortex_m::peripheral::SCB::sys_reset();
                }
                if running {
                    start_stored_link();
                }
                ok
            } else {
                let mut n = new;
                n.mode = settings::get().mode; // a mode change needs a reboot: persist it
                settings::set_volatile(&n);
                true
            };
            out[0] = EV_FW;
            out[1] = cmd[0];
            out[2] = ok as u8;
            return 3;
        }
        CMD_PAIR_START => ui::PAIR_REQUEST.signal(()),
        CMD_PAIR_EXPORT => {
            let p = pairing::load();
            out[0] = EV_PAIR_EXPORT;
            out[1] = p.is_some() as u8;
            let p = p.unwrap_or(pairing::Pairing { network_id: 0, key: [0; 16] });
            out[2..6].copy_from_slice(&p.network_id.to_le_bytes());
            out[6..22].copy_from_slice(&p.key);
            return 22;
        }
        CMD_PAIR_FORGET => {
            radio::stop();
            pairing::forget();
        }
        CMD_STATUS => {
            out[0] = EV_STATUS;
            out[1] = settings::get().mode as u8;
            out[2] = pairing::load().is_some() as u8;
            out[3] = ui::pair_state() as u8;
            let slots = link::slots();
            out[4] = slots.len() as u8;
            let mut n = 5;
            for &(slot, right, _) in &slots {
                out[n] = slot;
                out[n + 1] = right as u8;
                out[n + 2] = gamepad::battery(slot).unwrap_or(255);
                n += 3;
            }
            for &(slot, _, _) in &slots {
                out[n] = gamepad::status(slot);
                n += 1;
            }
            return n;
        }
        CMD_RUMBLE if a.len() >= 2 => link::rumble(a[0], a[1]),
        CMD_LINK_REQ if a.len() >= 3 => {
            out[0] = EV_DM_SENT;
            out[1] = link::request(a[0], a[1], a[2] != 0, &a[3..]) as u8;
            return 2;
        }
        CMD_FW_BEGIN | CMD_FW_DATA | CMD_FW_COMMIT if a.len() >= 4 => {
            let ok = match cmd[0] {
                CMD_FW_BEGIN => selfupdate::begin(rd_u32(a) as usize),
                CMD_FW_DATA => selfupdate::data(rd_u32(a) as usize, &a[4..]),
                _ if a.len() >= 8 => {
                    radio::stop();
                    selfupdate::commit(rd_u32(a) as usize, rd_u32(&a[4..]))
                }
                _ => false,
            };
            out[0] = EV_FW;
            out[1] = cmd[0];
            out[2] = ok as u8;
            return 3;
        }
        CMD_PAD_SET => {
            gamepad::set(a);
        }
        CMD_DFU => enter_dfu(),
        _ => {}
    }
    0
}

async fn session(tx: &mut Sender<'static, UsbDriver>, rx: &mut Receiver<'static, UsbDriver>) -> Result<(), EndpointError> {
    let mut pkt = [0u8; 64];
    let mut acc = [0u8; 300];
    let mut acc_len = 0usize;
    let mut out = [0u8; 300];
    loop {
        match select(rx.read_packet(&mut pkt), radio::EVENTS.receive()).await {
            Either::First(n) => {
                for &b in &pkt[..n?] {
                    if b != 0 {
                        if acc_len < acc.len() {
                            acc[acc_len] = b;
                            acc_len += 1;
                        }
                        continue;
                    }
                    let mut dec = [0u8; 300];
                    let len = cobs::decode(&acc[..acc_len], &mut dec);
                    acc_len = 0;
                    if let Some(len) = len {
                        let n = handle(&dec[..len], &mut out);
                        if n > 0 {
                            send_frame(tx, &out[..n]).await?;
                        }
                    }
                }
            }
            Either::Second(ev) => {
                let n = match ev {
                    radio::Event::Rx { ts, crc_ok, rxmatch, rssi, data } => {
                        out[0] = EV_RX;
                        out[1..5].copy_from_slice(&ts.to_le_bytes());
                        out[5] = crc_ok as u8;
                        out[6] = rxmatch;
                        out[7] = rssi;
                        out[8..8 + data.len()].copy_from_slice(&data);
                        8 + data.len()
                    }
                    radio::Event::TxDone { ts } => {
                        out[0] = EV_TXDONE;
                        out[1..5].copy_from_slice(&ts.to_le_bytes());
                        5
                    }
                    radio::Event::DmLink { connected, id0, id1 } => {
                        out[0] = EV_DM_LINK;
                        out[1] = connected as u8;
                        out[2..6].copy_from_slice(&id0.to_le_bytes());
                        out[6..10].copy_from_slice(&id1.to_le_bytes());
                        10
                    }
                    radio::Event::DmResp { hdr, seq, data } => {
                        out[0] = EV_DM_RESP;
                        out[1] = hdr;
                        out[2] = seq;
                        out[3..3 + data.len()].copy_from_slice(&data);
                        3 + data.len()
                    }
                    radio::Event::LinkJoin { slot, id, dev_type } => {
                        out[0] = EV_LINK_JOIN;
                        out[1] = slot;
                        out[2..10].copy_from_slice(&id);
                        out[10..14].copy_from_slice(&dev_type.to_le_bytes());
                        14
                    }
                    radio::Event::LinkDrop { slot } => {
                        out[0] = EV_LINK_DROP;
                        out[1] = slot;
                        2
                    }
                    radio::Event::PairStatus(state) => {
                        out[0] = EV_PAIR_STATUS;
                        out[1] = state;
                        2
                    }
                    radio::Event::LinkData { slot, rssi, mic_ok, data } => {
                        out[0] = EV_LINK_DATA;
                        out[1] = slot;
                        out[2] = rssi;
                        out[3] = mic_ok as u8;
                        out[4..4 + data.len()].copy_from_slice(&data);
                        4 + data.len()
                    }
                };
                send_frame(tx, &out[..n]).await?;
            }
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    config.lfclk_source = embassy_nrf::config::LfclkSource::Synthesized;
    let p = embassy_nrf::init(config);

    wdt::start();
    let mode = settings::load().mode;
    radio::init();
    start_stored_link();

    // Front panel: LED2 (RGB, active low) on PWM0 as channels R, G, B, and the SW1 button.
    // Green / blue pins verified on hardware: P0.12 lights green, P1.09 blue.
    let mut pwm_cfg = SimpleConfig::default();
    pwm_cfg.max_duty = 1000;
    pwm_cfg.prescaler = Prescaler::Div16;
    pwm_cfg.ch0_idle_level = Level::High;
    pwm_cfg.ch1_idle_level = Level::High;
    pwm_cfg.ch2_idle_level = Level::High;
    let pwm = SimplePwm::new_3ch(p.PWM0, p.P0_08, p.P0_12, p.P1_09, &pwm_cfg);
    spawner.spawn(ui::led_task(pwm).unwrap());
    spawner.spawn(ui::button_task(Input::new(p.P1_06, Pull::Up)).unwrap());
    spawner.spawn(ui::pair_task().unwrap());
    spawner.spawn(wdt::task().unwrap());

    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));
    // DS4 mode copies a real DS4's strings: some games (e.g. InControl's Unity profiles) match on the name.
    let (vid, pid, manufacturer, product) = match mode {
        settings::Mode::Ds4 => (ds4::VID, ds4::PID, "Sony Interactive Entertainment", "Wireless Controller"),
        settings::Mode::XInput => (xinput::VID, xinput::PID, "QuestPad", "QuestPad (XInput mode)"),
        settings::Mode::Hid => (0x1209, 0x5151, "QuestPad", "QuestPad"),
    };
    let mut ucfg = embassy_usb::Config::new(vid, pid);
    if mode == settings::Mode::XInput {
        xinput::configure(&mut ucfg);
    }
    ucfg.manufacturer = Some(manufacturer);
    ucfg.product = Some(product);
    ucfg.serial_number = Some("QCD-0001");
    ucfg.max_power = 100;
    ucfg.max_packet_size_0 = 64;

    static CONFIG_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 256]> = StaticCell::new();
    static MSOS_DESC: StaticCell<[u8; 128]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 128]> = StaticCell::new();
    static CDC_STATE: StaticCell<State> = StaticCell::new();
    static HID_STATE: StaticCell<hid::State> = StaticCell::new();

    let mut builder = Builder::new(
        driver,
        ucfg,
        CONFIG_DESC.init([0; 256]),
        BOS_DESC.init([0; 256]),
        MSOS_DESC.init([0; 128]),
        CONTROL_BUF.init([0; 128]),
    );
    static USB_EVENTS: StaticCell<UsbEvents> = StaticCell::new();
    builder.handler(USB_EVENTS.init(UsbEvents));
    let class = match mode {
        settings::Mode::XInput => {
            let eps = xinput::build(&mut builder);
            spawner.spawn(xinput_in_task(eps.ep_in).unwrap());
            spawner.spawn(xinput_out_task(eps.ep_out).unwrap());
            None
        }
        settings::Mode::Ds4 => {
            // HID first (interface 0, like a real pad), then the serial port for the GUI / tools.
            static DS4_HANDLER: StaticCell<ds4::Handler> = StaticCell::new();
            let hid_cfg = hid::Config {
                report_descriptor: ds4::REPORT_DESCRIPTOR,
                request_handler: Some(DS4_HANDLER.init(ds4::Handler)),
                poll_ms: 1,
                max_packet_size: 64,
                hid_subclass: hid::HidSubclass::No,
                hid_boot_protocol: hid::HidBootProtocol::None,
            };
            let pad = HidReaderWriter::<_, { ds4::OUT_LEN }, { ds4::IN_LEN }>::new(
                &mut builder,
                HID_STATE.init(hid::State::new()),
                hid_cfg,
            );
            let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);
            let (pad_rx, pad_tx) = pad.split();
            spawner.spawn(ds4_task(pad_tx).unwrap());
            spawner.spawn(ds4_out_task(pad_rx).unwrap());
            Some(class)
        }
        _ => {
            let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), 64);
            static RUMBLE: StaticCell<gamepad::Rumble> = StaticCell::new();
            let hid_cfg = hid::Config {
                report_descriptor: gamepad::REPORT_DESCRIPTOR,
                request_handler: Some(RUMBLE.init(gamepad::Rumble)),
                poll_ms: 1,
                max_packet_size: 64,
                hid_subclass: hid::HidSubclass::No,
                hid_boot_protocol: hid::HidBootProtocol::None,
            };
            let pad = HidReaderWriter::<_, { gamepad::OUT_REPORT_LEN }, { gamepad::REPORT_LEN }>::new(
                &mut builder,
                HID_STATE.init(hid::State::new()),
                hid_cfg,
            );
            let (pad_rx, pad_tx) = pad.split();
            spawner.spawn(hid_task(pad_tx).unwrap());
            spawner.spawn(rumble_task(pad_rx).unwrap());
            Some(class)
        }
    };
    let dev = builder.build();
    spawner.spawn(usb_task(dev).unwrap());
    spawner.spawn(suspend_task().unwrap());

    let Some(class) = class else {
        // XInput mode: no serial port; the other tasks do all the work.
        core::future::pending::<()>().await;
        unreachable!()
    };
    let (mut tx, mut rx) = class.split();
    loop {
        rx.wait_connection().await;
        let _ = session(&mut tx, &mut rx).await;
        // A host tool exited: drop any bridge-driven mode, but keep (or resume) the standalone link
        // (and leave dongle-driven pairing alone).
        let pairing = matches!(ui::pair_state(), ui::PairState::Waiting | ui::PairState::Writing);
        if !link::running() && !pairing {
            radio::stop();
            start_stored_link();
        }
        Timer::after(Duration::from_millis(10)).await;
    }
}
