@ The accelerator's nonce loop. Each nonce takes three blocks, and most of the time
@ goes on the bus, at 4 to 5 cycles per register access. The core has nothing to do
@ during the 57 cycles of each digest, so all the RAM reads and the loop's own work
@ happen there, and the rest is only register accesses.
@
@ A status read that comes too early costs about 6 cycles, so each digest ends with
@ a measured wait that puts the first read right after the digest. Two nops take one
@ cycle. These waits only tune the speed: the status is always read, so a late write
@ on the bus delays the loop without breaking it. Measure again after any change, as
@ moving a single instruction shifts them.
@
@ r0 job words, r1 nonce, r2 count, r3 accelerator, lr limit, r12 scratch.

.syntax unified
.thumb

.equ SHA256, 0x400f8000
.equ CSR, 0x00
.equ WDATA, 0x04
.equ SUM0, 0x08
.equ SUM7, 0x24
.equ START, 1 << 0
.equ WDATA_RDY, 1 << 1
.equ SUM_VLD, 1 << 2
.equ BLOCK_BYTES, 64
.equ FIRST_MESSAGE_BITS, 640
.equ SECOND_MESSAGE_BITS, 256
.equ PADDING, 0x80000000

.macro put reg:req, more:vararg
    str \reg, [r3, #WDATA]
    .ifnb \more
    put \more
    .endif
.endm

.macro idle nops:req
    .rept \nops
    nop
    .endr
.endm

.macro poll flag:req
1:
    ldr.w r12, [r3, #CSR]
    tst.w r12, #\flag
    beq 1b
.endm

@ Clears BSWAP along the way: the words here are message words already.
.macro restart
    mov.w r12, #START
    str.w r12, [r3, #CSR]
.endm

@ The nonce has moved on by the time its second digest is done.
.macro check_hit
    poll SUM_VLD
    ldr.w r12, [r3, #SUM7]
    rev.w r12, r12
    cmp r12, lr
    bls found
.endm

.section .data.ram_func.picominer_scan_accelerator, "ax", %progbits
.global picominer_scan_accelerator
.type picominer_scan_accelerator, %function
.thumb_func
.p2align 2
picominer_scan_accelerator:
    push {r4-r11, lr}
    mov lr, r3
    movw r3, #:lower16:SHA256
    movt r3, #:upper16:SHA256
    ldm r0, {r4-r11}
    b first_nonce

next_nonce:
    check_hit
first_nonce:
    restart
    put r4, r5, r6, r7, r8, r9, r10, r11
    add.w r12, r0, #BLOCK_BYTES / 2
    ldm r12, {r4-r11}
    put r4, r5, r6, r7, r8, r9, r10, r11

    add.w r12, r0, #BLOCK_BYTES
    ldm r12, {r4-r6}
    rev r7, r1
    mov.w r8, #PADDING
    mov.w r9, #0
    mov.w r10, #FIRST_MESSAGE_BITS
    idle 14
    poll WDATA_RDY
    put r4, r5, r6, r7, r8
    put r9, r9, r9, r9, r9, r9, r9, r9, r9, r9
    put r10

    add.w r4, r3, #SUM0
    idle 10
    poll SUM_VLD
    ldm r4, {r4-r11}
    restart
    put r4, r5, r6, r7, r8, r9, r10, r11
    mov.w r4, #PADDING
    movs r5, #0
    mov.w r6, #SECOND_MESSAGE_BITS
    put r4, r5, r5, r5, r5, r5, r5, r6

    ldm r0, {r4-r11}
    adds r1, #1
    subs r2, #1
    idle 16
    bne next_nonce

    check_hit
    movs r0, #0
    pop {r4-r11, pc}

found:
    subs r1, #1
    movs r0, #1
    pop {r4-r11, pc}
