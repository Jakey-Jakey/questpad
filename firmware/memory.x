/* nRF52840 Dongle (PCA10059): MBR at 0x0, app at 0x1000, Open USB DFU bootloader at 0xE0000.
   0xDE000 = settings (src/settings.rs), 0xDF000 = pairing record (src/pairing.rs). */
MEMORY
{
  FLASH : ORIGIN = 0x00001000, LENGTH = 0xDD000
  RAM   : ORIGIN = 0x20000008, LENGTH = 0x3FFF8
}
