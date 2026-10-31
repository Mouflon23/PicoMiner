MEMORY {
    /* The Pico 2 W has 4 MB. The last 4K sector holds the portal's settings, see
       settings.rs. */
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K - 4K
    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
}

/* The boot ROM looks for the image definition in the first 4K of flash. */
SECTIONS {
    .start_block : ALIGN(4) {
        KEEP(*(.start_block));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);
