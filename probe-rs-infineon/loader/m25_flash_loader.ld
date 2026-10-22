/* M25-4b flash loader: code runs from PSPR, data in DSPR */
ENTRY(_start)
MEMORY
{
  psprtext (rx) : ORIGIN = 0x70100000, LENGTH = 8K
  dsprdata (rw) : ORIGIN = 0x7000E000, LENGTH = 2K
}

SECTIONS
{
  /* _start first: the host sets PC = 0x70100000 */
  .text : { *(.text.entry) *(.text*) *(.rodata*) } > psprtext
  /* above the payload buffer (0x7000A000-0x7000BFFF), below the mailbox */
  .data : { *(.data*) } > dsprdata
  .bss : { *(.bss*) *(COMMON) } > dsprdata
}
