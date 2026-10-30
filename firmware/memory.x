MEMORY {
    BOOT2 : ORIGIN = 0x10000000, LENGTH = 0x100
    FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 0x100

    /* The unstriped aliases of the four 64K SRAM banks. Each core hashes with its
       code and stack alone in one bank, so the cores never wait for each other. */
    RAM    : ORIGIN = 0x21000000, LENGTH = 128K
    MINER0 : ORIGIN = 0x21020000, LENGTH = 64K
    MINER1 : ORIGIN = 0x21030000, LENGTH = 64K
}

/* Core 0 runs everything on this stack, including interrupt handlers. */
_stack_start = ORIGIN(MINER0) + LENGTH(MINER0);

SECTIONS {
    .miner0 : ALIGN(4) {
        __miner0_start = .;
        KEEP(*(.miner0.text))
        . = ALIGN(4);
        __miner0_end = .;
    } > MINER0 AT > FLASH
    __miner0_load = LOADADDR(.miner0);

    .miner1 : ALIGN(4) {
        __miner1_start = .;
        KEEP(*(.miner1.text))
        . = ALIGN(4);
        __miner1_end = .;
    } > MINER1 AT > FLASH
    __miner1_load = LOADADDR(.miner1);

    .miner1_stack (NOLOAD) : ALIGN(8) {
        *(.miner1.stack)
    } > MINER1
/* Not AFTER .data: that would push __edata, and the startup code would copy
   these regions, and everything between them, over the running stack. */
} INSERT BEFORE .data;
