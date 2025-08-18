/* Minimal linker script for position-independent CRC binary */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 4K
}

SECTIONS
{
  .text 0x00000000 :
  {
    *(.text._start)
    *(.text.calculate_crc32)
    *(.text .text.*)
    . = ALIGN(4);
    *(.rodata .rodata.*)
  } > FLASH
}
