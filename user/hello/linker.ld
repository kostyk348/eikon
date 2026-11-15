/* A plain static ELF at a fixed physical address: the loader copies PT_LOAD
   segments to their p_vaddr, so the program must be linked where it will run. */
ENTRY(_start)
SECTIONS
{
    . = 0x84000000;
    .text : { *(.text.entry) *(.text*) }
    .rodata : { *(.rodata*) }
    .data : { *(.data*) }
    .bss : { *(.bss*) *(COMMON) }
}
