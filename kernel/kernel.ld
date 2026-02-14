ENTRY(_start);

SECTIONS
{
  . = 2M;

  .bootloader-config : ALIGN(4K) { KEEP(*(.bootloader-config)) }
  .text : ALIGN(4K) { *(.text .text.*) }
  .rodata : ALIGN(4K) { *(.rodata .rodata.*) *(.srodata .srodata.*) }
  .data : ALIGN(4K) {
    *(.data.rel.ro .data.rel.ro.*)
    *(.got .got.*)
    *(.data .data.*)
    *(.sdata .sdata.*)
  }
  .bss : ALIGN(4K) { *(.bss .bss.*) *(.sbss .sbss.*) *(COMMON) }
}
