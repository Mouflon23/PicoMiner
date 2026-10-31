#!/usr/bin/env python3
"""Packs the firmware for both boards into one UF2.

Every block of a UF2 names the chip family it is for, and a boot ROM skips the
others. So one file holding both images works on a Pico W and on a Pico 2 W, and
each board picks its own firmware by itself.

    python tools/uf2.py picominer.uf2 rp2040.elf rp2350.elf
"""

import struct
import sys

RP2040 = 0xe48bff56
RP2350_ARM_SECURE = 0xe48bff59

MAGIC_START0 = 0x0a324655
MAGIC_START1 = 0x9e5d5157
MAGIC_END = 0x0ab16f30
FLAG_FAMILY_PRESENT = 0x2000

FLASH_START = 0x10000000
FLASH_END = 0x11000000
PAGE = 256


def flash_pages(path):
    """Returns {page address: page bytes} for the loadable parts of a 32-bit ELF."""
    with open(path, "rb") as file:
        elf = file.read()
    if elf[:6] != b"\x7fELF\x01\x01":
        sys.exit(f"{path}: not a little-endian 32-bit ELF")
    phoff = struct.unpack_from("<I", elf, 28)[0]
    phentsize, phnum = struct.unpack_from("<HH", elf, 42)

    pages = {}
    for i in range(phnum):
        kind, offset, _, paddr, filesz = struct.unpack_from("<5I", elf, phoff + i * phentsize)
        if kind != 1 or filesz == 0:
            continue
        if not FLASH_START <= paddr < FLASH_END:
            sys.exit(f"{path}: segment at {paddr:#x} is outside flash")
        for at in range(filesz):
            address = paddr + at
            page = pages.setdefault(address & ~(PAGE - 1), bytearray(PAGE))
            page[address & (PAGE - 1)] = elf[offset + at]
    return pages


def blocks(pages, family):
    """Returns the 512-byte blocks of one image, numbered from zero."""
    out = []
    for number, address in enumerate(sorted(pages)):
        head = struct.pack(
            "<8I",
            MAGIC_START0,
            MAGIC_START1,
            FLAG_FAMILY_PRESENT,
            address,
            PAGE,
            number,
            len(pages),
            family,
        )
        out.append(head + bytes(pages[address]) + bytes(476 - PAGE) + struct.pack("<I", MAGIC_END))
    return out


def interleave(*images):
    """Mixes the images evenly. A board restarts on its last block, and if that came
    in the middle of the file, Windows would report a failed copy."""
    tagged = [((i + 1) / len(image), block) for image in images for i, block in enumerate(image)]
    tagged.sort(key=lambda item: item[0])
    return [block for _, block in tagged]


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    output, rp2040, rp2350 = sys.argv[1:]
    with open(output, "wb") as file:
        file.writelines(
            interleave(
                blocks(flash_pages(rp2350), RP2350_ARM_SECURE),
                blocks(flash_pages(rp2040), RP2040),
            )
        )


if __name__ == "__main__":
    main()
