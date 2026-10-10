//! XInput mode: present as a wired Xbox 360 controller (045E:028E), which Windows' built-in
//! `xusb22` driver (native XInput, with rumble) and Linux's `xpad` pick up with no setup.
//!
//! The configuration mirrors a wired 360 pad (layout as in GP2040-CE's XInputDescriptors.h,
//! MIT, Copyright (c) 2021 Jason Skuby): gamepad (FF/5D/01), audio (FF/5D/03), plugin module
//! (FF/5D/02) and security (FF/FD/13) interfaces. Only the gamepad interface is serviced; the
//! others just NAK. Remote wakeup is deliberately not advertised (so hosts don't selectively
//! suspend it: suspend powers the controllers off). The device class is vendor-specific, so
//! Windows gives the whole device to xusb22: there is no serial port in this mode (switch modes
//! with the dongle button).
//!
//! Input (endpoint 0x81, 20 bytes): [0x00][0x14][buttons1][buttons2][LT][RT][LX][LY][RX][RY]
//! (i16 LE, +y = up)[6 reserved]. Output (endpoint 0x02): [0x00][0x08][0x00][left / big motor]
//! [right / small motor]... = rumble; [0x01][0x03][pattern] = ring LED (ignored).

use embassy_time::{with_timeout, Duration};
use embassy_usb::driver::{Direction, EndpointAddress, EndpointIn, EndpointOut};
use embassy_usb::Builder;

use crate::gamepad::{self, Pad};

pub const VID: u16 = 0x045E;
pub const PID: u16 = 0x028E;
pub const DEVICE_RELEASE: u16 = 0x0114;

/// Class-specific descriptors (type 0x21 / 0x41, without their length / type bytes).
/// Gamepad: subtype gamepad, IN 0x81 (20-byte reports), OUT 0x02 (8-byte reports).
const GAMEPAD_DESC: [u8; 15] = [0x00, 0x01, 0x01, 0x25, 0x81, 0x14, 0x00, 0x00, 0x00, 0x00, 0x13, 0x02, 0x08, 0x00, 0x00];
/// Audio (headset): mic IN 0x83, audio OUT 0x04, unknown IN 0x85 / OUT 0x06.
const AUDIO_DESC: [u8; 25] = [
    0x00, 0x01, 0x01, 0x01, 0x83, 0x40, 0x01, 0x04, 0x20, 0x16, 0x85, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x06,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
/// Plugin module: IN 0x86.
const PLUGIN_DESC: [u8; 7] = [0x00, 0x01, 0x01, 0x22, 0x86, 0x03, 0x00];
/// Security (XSM3; used by the 360 console only).
const SECURITY_DESC: [u8; 4] = [0x00, 0x01, 0x01, 0x03];

pub fn configure(cfg: &mut embassy_usb::Config) {
    cfg.device_class = 0xFF;
    cfg.device_sub_class = 0xFF;
    cfg.device_protocol = 0xFF;
    cfg.device_release = DEVICE_RELEASE;
    cfg.composite_with_iads = false;
}

pub struct Endpoints<D: embassy_usb::driver::Driver<'static>> {
    pub ep_in: D::EndpointIn,
    pub ep_out: D::EndpointOut,
}

pub fn build<D: embassy_usb::driver::Driver<'static>>(builder: &mut Builder<'static, D>) -> Endpoints<D> {
    let ep = |n, d| Some(EndpointAddress::from_parts(n, d));
    let mut func = builder.function(0xFF, 0x5D, 0x01);
    // 0: gamepad
    let mut iface = func.interface();
    let mut alt = iface.alt_setting(0xFF, 0x5D, 0x01, None);
    alt.descriptor(0x21, &GAMEPAD_DESC);
    let ep_in = alt.endpoint_interrupt_in(ep(1, Direction::In), 32, 1);
    let ep_out = alt.endpoint_interrupt_out(ep(2, Direction::Out), 32, 8);
    // 1: audio (unserviced)
    let mut iface = func.interface();
    let mut alt = iface.alt_setting(0xFF, 0x5D, 0x03, None);
    alt.descriptor(0x21, &AUDIO_DESC);
    let _ = alt.endpoint_interrupt_in(ep(3, Direction::In), 32, 2);
    let _ = alt.endpoint_interrupt_out(ep(4, Direction::Out), 32, 4);
    let _ = alt.endpoint_interrupt_in(ep(5, Direction::In), 32, 64);
    let _ = alt.endpoint_interrupt_out(ep(6, Direction::Out), 32, 16);
    // 2: plugin module (unserviced)
    let mut iface = func.interface();
    let mut alt = iface.alt_setting(0xFF, 0x5D, 0x02, None);
    alt.descriptor(0x21, &PLUGIN_DESC);
    let _ = alt.endpoint_interrupt_in(ep(6, Direction::In), 32, 16);
    // 3: security (no endpoints)
    let mut iface = func.interface();
    let mut alt = iface.alt_setting(0xFF, 0xFD, 0x13, None);
    alt.descriptor(0x41, &SECURITY_DESC);
    Endpoints { ep_in, ep_out }
}

fn neg(v: i16) -> i16 {
    v.saturating_neg()
}

pub fn report(p: &Pad) -> [u8; 20] {
    use gamepad::*;
    let b = p.buttons;
    let on = |m: u16, v: u8| if b & m != 0 { v } else { 0 };
    let mut r = [0u8; 20];
    r[0] = 0x00;
    r[1] = 0x14;
    r[2] = on(BTN_START, 0x10) | on(BTN_BACK, 0x20) | on(BTN_LS, 0x40) | on(BTN_RS, 0x80);
    r[3] = on(BTN_LB, 0x01)
        | on(BTN_RB, 0x02)
        | on(BTN_GUIDE, 0x04)
        | on(BTN_A, 0x10)
        | on(BTN_B, 0x20)
        | on(BTN_X, 0x40)
        | on(BTN_Y, 0x80);
    r[4] = p.lt;
    r[5] = p.rt;
    // XInput: +y is up (our pad state uses HID's +y = down).
    for (i, v) in [p.lx, neg(p.ly), p.rx, neg(p.ry)].iter().enumerate() {
        r[6 + 2 * i..8 + 2 * i].copy_from_slice(&v.to_le_bytes());
    }
    r
}

/// Stream input reports (on change, and at least every 8 ms).
pub async fn run_in<E: EndpointIn>(mut ep: E) -> ! {
    let mut pad = Pad::default();
    loop {
        ep.wait_enabled().await;
        if let Ok(p) = with_timeout(Duration::from_millis(8), gamepad::PAD.wait()).await {
            pad = p;
        }
        let _ = ep.write(&report(&pad)).await;
    }
}

/// Rumble commands from the host.
pub async fn run_out<E: EndpointOut>(mut ep: E) -> ! {
    let mut buf = [0u8; 32];
    loop {
        ep.wait_enabled().await;
        if let Ok(n) = ep.read(&mut buf).await {
            if n >= 5 && buf[0] == 0x00 && buf[1] >= 0x08 {
                crate::link::rumble(buf[3], buf[4]);
            }
        }
    }
}
