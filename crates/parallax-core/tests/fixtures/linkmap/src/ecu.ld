/* A small microcontroller layout: code and constants in flash, data copied to RAM. */
ENTRY(reset_handler)
MEMORY
{
  FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 256K
  RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 64K
  BKPRAM (rw) : ORIGIN = 0x40024000, LENGTH = 4K
}
SECTIONS
{
  .isr_vector : { KEEP(*(.isr_vector)) } > FLASH
  .text : { *(.text .text.*) *(.rodata .rodata.*) _etext = .; } > FLASH
  .data : { _sdata = .; *(.data .data.*) _edata = .; } > RAM AT > FLASH
  .bss (NOLOAD) : { _sbss = .; *(.bss .bss.*) *(COMMON) _ebss = .; } > RAM
  .backup (NOLOAD) : { *(.backup) } > BKPRAM
  /DISCARD/ : { *(.comment) *(.note.*) *(.eh_frame) }
}
