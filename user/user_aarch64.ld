ENTRY(_start);

SECTIONS
{
    . = 0x0000000400000000;

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
