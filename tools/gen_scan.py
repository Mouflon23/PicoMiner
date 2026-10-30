#!/usr/bin/env python3
"""Generates the Cortex-M0+ nonce loop and proves it on the host.

Writes firmware/src/scan.s and core/src/scan_context.rs. Every instruction is
also kept as data, so the same program runs on a small Thumb simulator here and
is compared against hashlib, with exact cycle counts, before going near a board.

    python tools/gen_scan.py

How the cycles are saved, compared with a plain SHA-256:

- The whole loop is unrolled, so rounds address W by a fixed stack offset and K
  by a pc-relative literal, and all 14 registers can hold state.
- Roles rotate instead of values: only three moves per round, because the high
  registers can still be read by MOV and ADD.
- Maj reuses a^b from the round before: 4 instructions instead of 7.
- The last rotation of one round and the first of the next share a count, as
  Sigma0 is factored to end on 13 and Sigma1 to start on it.
- The round that takes the nonce only adds it to two precomputed words, and the
  first round of the second hash does the same with its first message word.
- Words known at build time fold into K. Words known per job fold into
  precomputed slots. Only what depends on the nonce is computed per nonce.
- The second hash stops at round 60, and skips the half of rounds 57 to 60 that
  only feeds the state words nobody reads.
"""

import hashlib
import os
import random
import struct
import sys

K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]
IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19]
M = 0xffffffff


def ror(x, n):
    return ((x >> n) | (x << (32 - n))) & M


def small0(x):
    return ror(x, 7) ^ ror(x, 18) ^ (x >> 3)


def small1(x):
    return ror(x, 17) ^ ror(x, 19) ^ (x >> 10)


class Program:
    """Instructions as tuples; `text` renders them and `Machine` executes them."""

    def __init__(self):
        self.ops = []

    def __getattr__(self, name):
        def emit(*args):
            self.ops.append((name, *args))

        return emit


# Word classes: a build-time literal, a named per-job value, or per-nonce.
def lit(value):
    return ("lit", value & M)


def job(name):
    return ("job", name)


VAR = ("var",)


class Frame:
    """Stack layout. Every slot stays below the 1020 byte reach of ldr [sp, #imm]."""

    def __init__(self):
        self.names = {}

    def __getitem__(self, name):
        return self.names.setdefault(name, len(self.names) * 4)


class Generator:
    MAX_SLOTS = 24

    def __init__(self):
        self.p = Program()
        self.frame = Frame()
        self.slots = []  # (terms, literal), the per-job precomputed words
        for name in ("nonce", "count", "limit"):
            self.frame[name]
        for i in range(8):
            self.frame[f"state3{i}"]
        for i in range(8):
            self.frame[f"mid{i}"]
        # Slots follow directly: the entry code copies the context in one run.
        for i in range(self.MAX_SLOTS):
            self.frame[f"slot{i}"]

    def slot(self, terms, literal):
        key = (tuple(sorted(terms)), literal & M)
        if key not in self.slots:
            self.slots.append(key)
            assert len(self.slots) <= self.MAX_SLOTS
        return f"slot{self.slots.index(key)}"

    def pool(self):
        """Literals only reach forward, so drop them in the code every so often."""
        self.p.b_skip_pool()

    def load_const(self, reg, value):
        if value < 256:
            self.p.movs_i(reg, value)
        else:
            self.p.ldr_lit(reg, value)

    def load_slot(self, reg, name):
        self.p.ldr_sp(reg, self.frame[name])

    def sigma(self, dst, src, count, shift_a, shift_b, shift_right):
        """dst = ror(src, a+b) ^ ror(src, b) ^ (src >> right); src is left intact."""
        p = self.p
        p.mov(dst, src)
        p.movs_i(count, shift_a)
        p.rors(dst, count)
        p.eors(dst, src)
        p.movs_i(count, shift_b)
        p.rors(dst, count)
        p.lsrs(count, src, shift_right)
        p.eors(dst, count)

    def schedule(self, prefix, words, end):
        """Expands `words` up to index `end`, storing what later rounds read."""
        p = self.p
        first = len(words)
        # Everything past the declared words depends on the nonce.
        kind = lambda i: words[i] if i < first else VAR
        fast = next(
            (i for i in range(first, end) if all(kind(i - d) == VAR for d in (2, 7, 15, 16))),
            end,
        )
        for i in range(first, fast):
            if (i - first) % 12 == 11:
                self.pool()
            literal = 0
            terms = []
            acc = None

            def add(reg):
                nonlocal acc
                if acc is None:
                    acc = reg
                else:
                    p.adds(acc, reg)

            w2 = words[i - 2]
            if w2[0] == "lit":
                literal += small1(w2[1])
            elif w2[0] == "job":
                terms.append(("small1", w2[1]))
            else:
                p.ldr_sp("r1", self.frame[f"{prefix}{i - 2}"])
                self.sigma("r0", "r1", "r2", 2, 17, 10)
                add("r0")

            w15 = words[i - 15]
            if w15[0] == "lit":
                literal += small0(w15[1])
            elif w15[0] == "job":
                terms.append(("small0", w15[1]))
            else:
                p.ldr_sp("r1", self.frame[f"{prefix}{i - 15}"])
                dst = "r0" if acc is None else "r3"
                self.sigma(dst, "r1", "r2", 11, 7, 3)
                add(dst)

            for d in (7, 16):
                w = words[i - d]
                if w[0] == "lit":
                    literal += w[1]
                elif w[0] == "job":
                    terms.append(("id", w[1]))
                else:
                    reg = "r0" if acc is None else "r4"
                    p.ldr_sp(reg, self.frame[f"{prefix}{i - d}"])
                    add(reg)

            literal &= M
            if acc is None:
                # Nothing per-nonce: this word is handled at round level.
                raise AssertionError(f"word {i} should have been predeclared")
            if terms:
                self.load_slot("r4", self.slot(terms, literal))
                p.adds(acc, "r4")
            elif literal:
                self.load_const("r4", literal)
                p.adds(acc, "r4")
            p.str_sp(acc, self.frame[f"{prefix}{i}"])
            words.append(VAR)

        self.pool()
        if fast >= end:
            return
        i = fast
        # Short-lived values stay in registers: w[i-1], w[i-2] and w[i-16] low,
        # w[i-3] to w[i-7] high, where ADD can still reach them.
        y, x1, x2 = "r0", "r1", "r2"
        p0, p1 = "r3", "r4"
        q, c = "r5", "r6"
        ring = ["r8", "r9", "r10", "r11", "r12"]
        p.ldr_sp(x1, self.frame[f"{prefix}{i - 1}"])
        p.ldr_sp(x2, self.frame[f"{prefix}{i - 2}"])
        p.ldr_sp(p1, self.frame[f"{prefix}{i - 16}"])
        for j in range(3, 8):
            p.ldr_sp(c, self.frame[f"{prefix}{i - j}"])
            p.mov(ring[(i - j) % 5], c)
        for i in range(fast, end):
            spot = ring[(i - 7) % 5]
            p.mov(y, x2)
            p.movs_i(c, 2)
            p.rors(y, c)
            p.eors(y, x2)
            p.movs_i(c, 17)
            p.rors(y, c)
            p.lsrs(c, x2, 10)
            p.eors(y, c)
            p.add(y, spot)  # w[i-7]
            p.mov(spot, x2)  # w[i-2] takes its place
            p.ldr_sp(p0, self.frame[f"{prefix}{i - 15}"])
            self.sigma(q, p0, c, 11, 7, 3)
            p.adds(y, q)
            p.adds(y, p1)
            p.str_sp(y, self.frame[f"{prefix}{i}"])
            words.append(VAR)
            y, x1, x2 = x2, y, x1
            p0, p1 = p1, p0

    def rounds(self, prefix, words, start, end, regs, skip_a_from=99):
        """Rounds start..=end; `regs` maps each role to its register."""
        p = self.p
        r = dict(regs)
        count_ready = False
        for i in range(start, end + 1):
            if (i - start) % 9 == 8:
                self.pool()
            a, b, c, d = r["a"], r["b"], r["c"], r["d"]
            e, f, g, h = r["e"], r["f"], r["g"], r["h"]
            y, t0, t1 = r["y"], r["t0"], r["t1"]

            p.mov(t0, e)
            if not count_ready:
                p.movs_i(t1, 13)
            p.rors(t0, t1)
            p.eors(t0, e)
            p.movs_i(t1, 14)
            p.rors(t0, t1)
            p.eors(t0, e)
            p.movs_i(t1, 11)
            p.rors(t0, t1)

            p.mov(t1, f)
            p.eors(t1, g)
            p.ands(t1, e)
            p.eors(t1, g)
            p.adds(t0, t1)

            w = words[i]
            if w[0] == "lit":
                self.load_const(t1, (K[i] + w[1]) & M)
                p.adds(t0, t1)
            elif w[0] == "job":
                self.load_slot(t1, self.slot([("id", w[1])], K[i]))
                p.adds(t0, t1)
            else:
                p.ldr_lit(t1, K[i])
                p.adds(t0, t1)
                p.ldr_sp(t1, self.frame[f"{prefix}{i}"])
                p.adds(t0, t1)
            p.add(t0, h)

            p.mov(h, g)
            p.mov(g, d)
            p.adds(g, t0)
            p.mov(d, b)

            if i < skip_a_from:
                p.mov(t1, a)
                p.eors(t1, b)
                p.ands(y, t1)
                p.eors(y, b)
                p.adds(t0, y)

                p.mov(y, a)
                p.movs_i(b, 12)
                p.rors(y, b)
                p.eors(y, a)
                p.movs_i(b, 9)
                p.rors(y, b)
                p.eors(y, a)
                p.movs_i(b, 13)
                p.rors(y, b)
                p.adds(t0, y)

            # b still holds the count 13, which starts the next round's Sigma1.
            count_ready = i < skip_a_from
            r = {
                "a": t0, "b": a, "c": d, "d": c,
                "e": g, "f": e, "g": f, "h": h,
                "y": t1, "t0": y, "t1": b,
            }
        return r

    def build(self):
        p = self.p
        frame = self.frame
        w1 = [job("w0"), job("w1"), job("w2"), VAR, lit(0x80000000)]
        w1 += [lit(0)] * 10 + [lit(640), job("w16"), job("w17")]
        w2 = [VAR] * 8 + [lit(0x80000000)] + [lit(0)] * 6 + [lit(256)]

        body = Program()
        self.p = p = body
        p.label("11")
        p.ldr_sp("r0", frame["nonce"])
        p.rev("r0", "r0")
        p.str_sp("r0", frame["a3"])
        self.schedule("a", w1, 64)

        regs = {
            "a": "r0", "b": "r1", "y": "r2", "e": "r3", "f": "r4", "g": "r5",
            "t0": "r6", "t1": "r7", "c": "r8", "d": "r9", "h": "r10",
        }
        for role, index in zip("abcdefgh", range(8)):
            reg = regs[role]
            if reg in ("r8", "r9", "r10"):
                p.ldr_sp("r6", frame[f"state3{index}"])
                p.mov(reg, "r6")
            else:
                p.ldr_sp(reg, frame[f"state3{index}"])
        p.ldr_sp("r6", frame["state32"])
        p.mov("r2", "r1")
        p.eors("r2", "r6")
        # Round 3 is already done but for the nonce, which it only adds in.
        p.ldr_sp("r6", frame["a3"])
        p.adds("r0", "r6")
        p.adds("r3", "r6")
        r = self.rounds("a", w1, 4, 63, regs)

        for role, index in zip("abcdefgh", range(8)):
            p.ldr_sp(r["t0"], frame[f"mid{index}"])
            p.add(r["t0"], r[role])
            p.str_sp(r["t0"], frame[f"b{index}"])
        self.pool()

        self.schedule("b", w2, 61)

        # Same for round 0 of the second hash, with its first message word.
        after_round0 = round_step(IV, K[0], 0)
        for role, value in zip("abcdefgh", after_round0):
            reg = regs[role]
            if reg in ("r8", "r9", "r10"):
                p.ldr_lit("r6", value)
                p.mov(reg, "r6")
            else:
                p.ldr_lit(reg, value)
        p.ldr_lit("r2", after_round0[1] ^ after_round0[2])
        p.ldr_sp("r6", frame["b0"])
        p.adds("r0", "r6")
        p.adds("r3", "r6")
        r = self.rounds("b", w2, 1, 60, regs, skip_a_from=57)
        self.pool()

        t0, t1 = r["t0"], r["t1"]
        p.ldr_lit(t0, IV[7])
        p.adds(t0, r["e"])
        p.rev(t0, t0)
        p.ldr_sp(t1, frame["limit"])
        p.cmp(t0, t1)
        p.bls("8")
        p.ldr_sp("r0", frame["nonce"])
        p.adds_i("r0", 1)
        p.str_sp("r0", frame["nonce"])
        p.ldr_sp("r0", frame["count"])
        p.subs_i("r0", 1)
        p.str_sp("r0", frame["count"])
        p.beq("9")
        p.bl("11")
        p.label("9")
        p.movs_i("r0", 0)
        p.b("7")
        p.label("8")
        p.movs_i("r0", 1)
        p.ldr_sp("r1", frame["nonce"])
        p.label("7")

        size = len(frame.names) * 4
        assert size <= 1020, size
        entry = Program()
        self.p = entry
        entry.push("r4", "r5", "r6", "r7", "lr")
        entry.mov("r4", "r8")
        entry.mov("r5", "r9")
        entry.mov("r6", "r10")
        entry.mov("r7", "r11")
        entry.push("r4", "r5", "r6", "r7")
        for _ in range((size + 507) // 508):
            entry.sub_sp(508)
        entry.str_sp("r1", frame["nonce"])
        entry.str_sp("r2", frame["count"])
        entry.str_sp("r3", frame["limit"])
        entry.add_rd_sp("r1", frame["state30"])
        entry.movs_i("r2", 16 + len(self.slots))
        entry.label("10")
        entry.ldmia("r0", "r3")
        entry.stmia("r1", "r3")
        entry.subs_i("r2", 1)
        entry.bne("10")

        leave = Program()
        for _ in range((size + 507) // 508):
            leave.add_sp(508)
        leave.pop("r4", "r5", "r6", "r7")
        leave.mov("r8", "r4")
        leave.mov("r9", "r5")
        leave.mov("r10", "r6")
        leave.mov("r11", "r7")
        leave.pop("r4", "r5", "r6", "r7", "pc")

        self.program = entry.ops + body.ops + leave.ops
        return self.program


def render(ops):
    out = []
    pool_count = 0
    for op, *a in ops:
        if op == "label":
            out.append(f"{a[0]}:")
        elif op == "b_skip_pool":
            out += ["    b 1f", "    .ltorg", "1:"]
            pool_count += 1
        elif op == "movs_i":
            out.append(f"    movs {a[0]}, #{a[1]}")
        elif op == "mov":
            out.append(f"    mov {a[0]}, {a[1]}")
        elif op in ("adds", "eors", "ands", "rors"):
            out.append(f"    {op} {a[0]}, {a[1]}")
        elif op == "add":
            out.append(f"    add {a[0]}, {a[1]}")
        elif op == "adds_i":
            out.append(f"    adds {a[0]}, #{a[1]}")
        elif op == "subs_i":
            out.append(f"    subs {a[0]}, #{a[1]}")
        elif op == "lsrs":
            out.append(f"    lsrs {a[0]}, {a[1]}, #{a[2]}")
        elif op == "rev":
            out.append(f"    rev {a[0]}, {a[1]}")
        elif op == "ldr_sp":
            out.append(f"    ldr {a[0]}, [sp, #{a[1]}]")
        elif op == "str_sp":
            out.append(f"    str {a[0]}, [sp, #{a[1]}]")
        elif op == "ldr_lit":
            out.append(f"    ldr {a[0]}, =0x{a[1]:08x}")
        elif op == "cmp":
            out.append(f"    cmp {a[0]}, {a[1]}")
        elif op in ("bls", "beq", "bne", "b"):
            direction = "b" if a[0] == "10" else "f"
            out.append(f"    {op} {a[0]}{direction}")
        elif op == "bl":
            out.append(f"    bl {a[0]}b")
        elif op in ("push", "pop"):
            out.append(f"    {op} {{{', '.join(a)}}}")
        elif op == "sub_sp":
            out.append(f"    sub sp, #{a[0]}")
        elif op == "add_sp":
            out.append(f"    add sp, #{a[0]}")
        elif op == "add_rd_sp":
            out.append(f"    add {a[0]}, sp, #{a[1]}")
        elif op == "ldmia":
            out.append(f"    ldmia {a[0]}!, {{{a[1]}}}")
        elif op == "stmia":
            out.append(f"    stmia {a[0]}!, {{{a[1]}}}")
        else:
            raise ValueError(op)
    return out


class Machine:
    """Just enough ARMv6-M, with the Cortex-M0+ timing on zero-wait SRAM."""

    COST = {
        "movs_i": 1, "mov": 1, "adds": 1, "add": 1, "adds_i": 1, "subs_i": 1, "eors": 1, "ands": 1,
        "rors": 1, "lsrs": 1, "rev": 1, "cmp": 1, "ldr_sp": 2, "str_sp": 2, "ldr_lit": 2,
        "sub_sp": 1, "add_sp": 1, "add_rd_sp": 1, "ldmia": 2, "stmia": 2, "b_skip_pool": 2,
        "bl": 3, "b": 2,
    }

    def __init__(self, ops):
        self.ops = ops
        self.labels = {op[1]: i for i, op in enumerate(ops) if op[0] == "label"}
        self.mem = {}
        self.cycles = 0

    def call(self, ctx_words, nonce, count, limit):
        self.regs = {f"r{i}": 0xdead0000 + i for i in range(13)}
        self.regs["lr"] = 0xfeedfeed
        for r in ("r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11"):
            self.regs[r] ^= 0x5555
        kept = {r: self.regs[r] for r in ("r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11")}
        self.regs["sp"] = 0x20040000
        ctx = 0x20000000
        for i, word in enumerate(ctx_words):
            self.mem[ctx + i * 4] = word
        self.regs["r0"], self.regs["r1"], self.regs["r2"], self.regs["r3"] = ctx, nonce, count, limit
        r = self.regs
        z = c = False
        pc = 0
        while True:
            op, *a = self.ops[pc]
            pc += 1
            if op == "label":
                continue
            self.cycles += self.COST.get(op, 0)
            if op == "movs_i":
                r[a[0]] = a[1]
            elif op == "mov":
                r[a[0]] = r[a[1]]
            elif op in ("adds", "add"):
                r[a[0]] = (r[a[0]] + r[a[1]]) & M
            elif op == "adds_i":
                r[a[0]] = (r[a[0]] + a[1]) & M
            elif op == "subs_i":
                r[a[0]] = (r[a[0]] - a[1]) & M
                z = r[a[0]] == 0
            elif op == "eors":
                r[a[0]] ^= r[a[1]]
            elif op == "ands":
                r[a[0]] &= r[a[1]]
            elif op == "rors":
                n = r[a[1]] & 31
                r[a[0]] = ror(r[a[0]], n) if n else r[a[0]]
            elif op == "lsrs":
                r[a[0]] = r[a[1]] >> a[2]
            elif op == "rev":
                r[a[0]] = struct.unpack(">I", struct.pack("<I", r[a[1]]))[0]
            elif op == "ldr_sp":
                r[a[0]] = self.mem[r["sp"] + a[1]]
            elif op == "str_sp":
                self.mem[r["sp"] + a[1]] = r[a[0]]
            elif op == "ldr_lit":
                r[a[0]] = a[1]
            elif op == "cmp":
                z = r[a[0]] == r[a[1]]
                c = r[a[0]] >= r[a[1]]
            elif op == "bls":
                taken = (not c) or z
                self.cycles += 2 if taken else 1
                if taken:
                    pc = self.labels[a[0]]
            elif op == "beq":
                self.cycles += 2 if z else 1
                if z:
                    pc = self.labels[a[0]]
            elif op == "bne":
                self.cycles += 1 if z else 2
                if not z:
                    pc = self.labels[a[0]]
            elif op in ("b", "bl"):
                if op == "bl":
                    r["lr"] = 0xbad0bad0
                pc = self.labels[a[0]]
            elif op == "b_skip_pool":
                pass
            elif op == "sub_sp":
                r["sp"] -= a[0]
            elif op == "add_sp":
                r["sp"] += a[0]
            elif op == "add_rd_sp":
                r[a[0]] = r["sp"] + a[1]
            elif op == "ldmia":
                r[a[1]] = self.mem[r[a[0]]]
                r[a[0]] += 4
            elif op == "stmia":
                self.mem[r[a[0]]] = r[a[1]]
                r[a[0]] += 4
            elif op == "push":
                self.cycles += 1 + len(a)
                r["sp"] -= 4 * len(a)
                for i, name in enumerate(a):
                    self.mem[r["sp"] + i * 4] = r[name]
            elif op == "pop":
                self.cycles += 1 + len(a) + (2 if "pc" in a else 0)
                for i, name in enumerate(a):
                    if name == "pc":
                        assert self.mem[r["sp"] + i * 4] == 0xfeedfeed, "lost the return address"
                    else:
                        r[name] = self.mem[r["sp"] + i * 4]
                r["sp"] += 4 * len(a)
                if "pc" in a:
                    for name, value in kept.items():
                        assert r[name] == value, f"{name} not restored"
                    return r["r0"], r["r1"]
            else:
                raise ValueError(op)


def compress(state, words):
    w = list(words)
    for i in range(16, 64):
        w.append((small1(w[i - 2]) + w[i - 7] + small0(w[i - 15]) + w[i - 16]) & M)
    s = list(state)
    for i in range(64):
        s = round_step(s, K[i], w[i])
    return [(x + y) & M for x, y in zip(state, s)]


def round_step(s, k, w):
    a, b, c, d, e, f, g, h = s
    big1 = ror(e, 6) ^ ror(e, 11) ^ ror(e, 25)
    t1 = (h + big1 + (g ^ (e & (f ^ g))) + k + w) & M
    big0 = ror(a, 2) ^ ror(a, 13) ^ ror(a, 22)
    t2 = (big0 + ((a & b) | (c & (a | b)))) & M
    return [(t1 + t2) & M, a, b, c, (d + t1) & M, e, f, g]


def context(gen, header):
    words = struct.unpack(">19I", header[:76])
    mid = compress(IV, words[:16])
    tail = words[16:19]
    state3 = list(mid)
    for i in range(3):
        state3 = round_step(state3, K[i], tail[i])
    named = {"w0": tail[0], "w1": tail[1], "w2": tail[2]}
    named["w16"] = (small0(tail[1]) + tail[0]) & M
    named["w17"] = (small1(640) + small0(tail[2]) + tail[1]) & M
    fns = {"id": lambda x: x, "small0": small0, "small1": small1}
    ctx = round_step(state3, K[3], 0) + mid
    for terms, literal in gen.slots:
        ctx.append((sum(fns[fn](named[name]) for fn, name in terms) + literal) & M)
    return ctx


def real_hash(header, nonce):
    return hashlib.sha256(hashlib.sha256(header[:76] + struct.pack("<I", nonce)).digest()).digest()


def verify(gen, ops):
    rng = random.Random(1)
    for case in range(60):
        header = bytes(rng.getrandbits(8) for _ in range(80))
        nonce = rng.getrandbits(32)
        top = struct.unpack("<I", real_hash(header, nonce)[28:])[0]
        ctx = context(gen, header)
        assert Machine(ops).call(ctx, nonce, 1, top) == (1, nonce), f"case {case}: missed"
        if top:
            assert Machine(ops).call(ctx, nonce, 1, top - 1)[0] == 0, f"case {case}: false hit"

    # A hit in the middle of a run stops there, and resumes at the right nonce.
    header = bytes(range(80))
    tops = [struct.unpack("<I", real_hash(header, n)[28:])[0] for n in range(40)]
    best = min(range(40), key=lambda n: tops[n])
    ctx = context(gen, header)
    assert Machine(ops).call(ctx, 0, 40, tops[best]) == (1, best)
    assert Machine(ops).call(ctx, best + 1, 39 - best, tops[best])[0] == 0

    one = Machine(ops)
    one.call(ctx, 0xffffffff, 1, 0)
    many = Machine(ops)
    # Bench nonces past any hit: limit 0 never matches this header in practice.
    many.call(ctx, 1000, 11, 0)
    return (many.cycles - one.cycles) // 10


def rust_context(gen):
    lines = [
        "// Generated by tools/gen_scan.py, do not edit.",
        "",
        f"pub const CONTEXT_WORDS: usize = {16 + len(gen.slots)};",
        "",
        "impl Work {",
        "    /// Everything the assembly nonce loop reads that does not change with the nonce.",
        "    pub fn context(&self) -> [u32; CONTEXT_WORDS] {",
        "        let [w0, w1, w2] = self.tail;",
        "        let (w16, w17) = (self.w16, self.w17);",
        "        let mut context = [0; CONTEXT_WORDS];",
        "        // Round 3 with a zero message word: the loop adds the nonce to a and e.",
        "        let [a, b, c, mut d, e, f, g, mut h] = self.state3;",
        "        round!(a, b, c, d, e, f, g, h, K[3], 0u32);",
        "        context[..8].copy_from_slice(&[h, a, b, c, d, e, f, g]);",
        "        context[8..16].copy_from_slice(&self.midstate);",
    ]
    used = set()
    for i, (terms, literal) in enumerate(gen.slots):
        parts = [name if fn == "id" else f"{fn}({name})" for fn, name in terms]
        used.update(name for _, name in terms)
        expr = f"0x{literal:08x}u32"
        for part in parts:
            expr += f".wrapping_add({part})"
        lines.append(f"        context[{16 + i}] = {expr};")
    lines += ["        let _ = (w0, w1, w2, w16, w17);", "        context", "    }", "}", ""]
    return "\n".join(lines)


def main():
    gen = Generator()
    ops = gen.build()
    cycles = verify(gen, ops)
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    body = "\n".join(render(ops))
    source = f"""// Generated by tools/gen_scan.py, do not edit.
//
// extern "C" fn(context: *const u32, nonce: u32, count: u32, limit: u32) -> u64
// Returns 1 in the low word and the nonce in the high word on a hit, else 0.
.syntax unified
.macro PICOMINER_SCAN name, section
.section \\section, "ax", %progbits
.global \\name
.type \\name, %function
.thumb_func
\\name:
{body}
    .ltorg
.size \\name, . - \\name
.endm

PICOMINER_SCAN picominer_scan_core0, .miner0.text
PICOMINER_SCAN picominer_scan_core1, .miner1.text
.text
"""
    with open(os.path.join(root, "firmware", "src", "scan.s"), "w", newline="\n") as f:
        f.write(source)
    with open(os.path.join(root, "core", "src", "scan_context.rs"), "w", newline="\n") as f:
        f.write(rust_context(gen))
    print(f"verified against hashlib; {cycles} cycles per hash in the simulator")
    print(f"{len(ops)} operations, {len(gen.slots)} per-job slots, frame {len(gen.frame.names) * 4} bytes")
    for mhz in (200, 250, 266, 300, 350):
        print(f"  {mhz} MHz: {mhz * 2e6 / cycles / 1000:.1f} kH/s on two cores")


if __name__ == "__main__":
    sys.exit(main())
