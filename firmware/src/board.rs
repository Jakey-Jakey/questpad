//! What differs between the boards the firmware runs on. The pins are taken in main.rs and the
//! flash layout is memory-<board>.x (picked by build.rs).
//!
//! Default: Nordic's nRF52840 Dongle (PCA10059). Open USB DFU bootloader, app at 0x1000, RGB LED,
//! button SW1 on P1.06.
//!
//! Feature `nicenano`: nice!nano v2. Adafruit UF2 bootloader, app at 0x26000 above the SoftDevice
//! it keeps, one blue LED on P0.15, no button (one wired from P1.06 to GND works like SW1).

/// GPREGRET value that keeps the bootloader in update mode across a reset (CMD_DFU).
#[cfg(not(feature = "nicenano"))]
pub const DFU_GPREGRET: u32 = 0xB1;
/// Adafruit's DFU_MAGIC_UF2_RESET: the bootloader shows its UF2 drive.
#[cfg(feature = "nicenano")]
pub const DFU_GPREGRET: u32 = 0x57;

/// Three channels (R, G, B) or one colour.
pub const RGB_LED: bool = cfg!(not(feature = "nicenano"));
pub const LED_ACTIVE_LOW: bool = cfg!(not(feature = "nicenano"));
