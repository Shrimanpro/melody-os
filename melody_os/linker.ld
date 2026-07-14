ENTRY(_start)

SECTIONS
{
    /* Tell the linker to put our code exactly where the Pi bootloader expects it */
    . = 0x80000;

    .text : {
        /* Force the _start function to be the very first bytes in the file */
        KEEP(*(.text._start))
        *(.text*)
    }

    .rodata : { *(.rodata*) }
    .data : { *(.data*) }
    
    .bss (NOLOAD) : {
        . = ALIGN(8);
        *(.bss*)
    }
}
