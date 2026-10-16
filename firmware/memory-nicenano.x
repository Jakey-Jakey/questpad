/* nice!nano v2: MBR at 0x0, S140 SoftDevice to 0x26000 (kept for the bootloader, never enabled: it
   forwards every interrupt and needs only RAM below 0x20000008), app at 0x26000, Adafruit UF2
   bootloader at 0xF4000. 0xDE000 = settings, 0xDF000 = pairing record, as on the dongle. */
MEMORY
{
  FLASH : ORIGIN = 0x00026000, LENGTH = 0xB8000
  RAM   : ORIGIN = 0x20000008, LENGTH = 0x3FFF8
}
