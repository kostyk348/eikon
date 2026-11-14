/* eikon: loaded by OpenSBI (qemu -machine virt -bios default) at 0x80200000 */
ENTRY(_start)

SECTIONS
{
    . = 0x80200000;

    .text : ALIGN(4) {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss (NOLOAD) : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    . = ALIGN(4096);
    .stack (NOLOAD) : ALIGN(16) {
        . += 0x40000;          /* 256 KiB kernel stack */
        __stack_top = .;
    }

    /DISCARD/ : { *(.eh_frame) *(.eh_frame_hdr) *(.note .note.*) }
}
