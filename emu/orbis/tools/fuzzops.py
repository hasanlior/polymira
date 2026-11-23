"""Builds complete GFX10.3 opcode tables by asking LLVM to name every opcode of every format.

Each candidate instruction is placed at a 16-byte stride, padded with s_nop, so the opcode of
any disassembled line is its address / 16.
"""
import glob, json, os, re, struct, subprocess, sys

sys.path.insert(0, os.path.dirname(__file__))
exec(open(os.path.join(os.path.dirname(__file__), 'mkelf.py')).read().split('for f in sorted')[0])

T = glob.glob(os.path.expanduser('~/.rustup/toolchains/*/lib/rustlib/x86_64-pc-windows-msvc/bin'))[0]
OBJDUMP = os.path.join(T, 'llvm-objdump.exe')
NOP = 0xBF800000

def name_all(words_for, count, variants=None):
    if variants:
        merged = {}
        for v in variants:
            for k, n in name_all(v, count).items():
                merged.setdefault(k, n)
        return merged
    code = bytearray()
    for op in range(count):
        ws = words_for(op)
        ws = ws + [NOP] * (4 - len(ws))
        code += struct.pack('<4I', *ws)
    path = 'fuzz.o'
    open(path, 'wb').write(elf(bytes(code)))
    out = subprocess.run([OBJDUMP, '-d', '--mcpu=gfx1013', path], capture_output=True, text=True).stdout
    names = {}
    for line in out.splitlines():
        m = re.match(r'\t(\S+).*//\s*([0-9A-F]+):', line)
        if not m:
            continue
        mn, addr = m.group(1), int(m.group(2), 16)
        if addr % 16 == 0 and mn not in ('.long', 'v_illegal') and not (mn == 's_nop' and addr % 16 == 0 and False):
            names[addr // 16] = mn
    return names

tables = {}
# SOP2: 10 op[29:23] sdst[22:16] ssrc1[15:8] ssrc0[7:0]   (s2 = s1 op s0)
tables['SOP2'] = name_all(lambda op: [(0b10 << 30) | (op << 23) | (2 << 16) | (1 << 8) | 0], 0x37)
# SOPK: 1011 op[27:23] sdst[22:16] simm16
tables['SOPK'] = name_all(lambda op: [(0b1011 << 28) | (op << 23) | (2 << 16) | 5], 32)
# SOP1: 101111101 sdst[22:16] op[15:8] ssrc0[7:0]
tables['SOP1'] = name_all(lambda op: [(0b101111101 << 23) | (2 << 16) | (op << 8) | 0], 256)
# SOPC: 101111110 op[22:16] ssrc1[15:8] ssrc0[7:0]
tables['SOPC'] = name_all(lambda op: [(0b101111110 << 23) | (op << 16) | (1 << 8) | 0], 128)
# SOPP: 101111111 op[22:16] simm16
tables['SOPP'] = name_all(lambda op: [(0b101111111 << 23) | (op << 16) | 0], 128)
# SMEM: 111101 op[25:18] glc[16] dlc[14] sdata[12:6] sbase[5:0] | offset[52:32] soff[63:57]
tables['SMEM'] = name_all(lambda op: [(0b111101 << 26) | (op << 18) | (4 << 6) | 0, 0xFA000000], 256)
# VOP2: 0 op[30:25] vdst[24:17] vsrc1[16:9] src0[8:0]
tables['VOP2'] = name_all(lambda op: [(op << 25) | (2 << 17) | (1 << 9) | 0x100], 64)
# VOP1: 0111111 vdst[24:17] op[16:9] src0[8:0]
tables['VOP1'] = name_all(lambda op: [(0b0111111 << 25) | (2 << 17) | (op << 9) | 0x100], 256)
# VOPC: 0111110 op[24:17] vsrc1[16:9] src0[8:0]
tables['VOPC'] = name_all(lambda op: [(0b0111110 << 25) | (op << 17) | (1 << 9) | 0x100], 256)
# VOP3: 110101 op[25:16] ... vdst[7:0] | src0[40:32] src1[49:41] src2[58:50]
tables['VOP3'] = name_all(None, 1024, [lambda op: [(0b110101 << 26) | (op << 16) | 2, 0x100 | (0x101 << 9) | (0x102 << 18)], lambda op: [(0b110101 << 26) | (op << 16) | 2, 0x100 | (0x101 << 9)], lambda op: [(0b110101 << 26) | (op << 16) | 2, 0x100], lambda op: [(0b110101 << 26) | (op << 16) | (0x6A << 8) | 2, 0x100 | (0x101 << 9) | (0x6A << 18)], lambda op: [(0b110101 << 26) | (op << 16) | 2, 0x100 | (0x101 << 9) | (0x102 << 18), ], lambda op: [(0b110101 << 26) | (op << 16) | 2, 0x2 | (0x3 << 9) | (0x4 << 18)]])
# VOP3P: 11001100 op[22:16] vdst[7:0] | src0 src1 src2
tables['VOP3P'] = name_all(lambda op: [(0b11001100 << 24) | (op << 16) | 2, 0x100 | (0x101 << 9) | (0x102 << 18) | (3 << 27)], 128)
# VINTRP: 110010 vdst[25:18] op[17:16] attr[15:10] chan[9:8] vsrc[7:0]
tables['VINTRP'] = name_all(lambda op: [(0b110010 << 26) | (2 << 18) | (op << 16) | (0 << 10) | 1], 4)
# DS: 110110 op[25:18] gds[17] offset1[15:8] offset0[7:0] | addr data0 data1 vdst
tables['DS'] = name_all(lambda op: [(0b110110 << 26) | (op << 18), 1 | (2 << 8) | (3 << 16) | (4 << 24)], 256)
# MUBUF: 111000 op[25:18] ... | vaddr[39:32] vdata[47:40] srsrc[52:48] soffset[63:56]   (op bit7 in [56]? gfx10 op[25:18])
tables['MUBUF'] = name_all(lambda op: [(0b111000 << 26) | ((op & 0x7F) << 18) | (((op >> 7) & 1) << 25), 1 | (2 << 8) | (1 << 16) | (0x80 << 24)], 256)
# MTBUF: 111010 dfmt/nfmt[25:19] op[18:16] ... op bit3 at [53]
tables['MTBUF'] = name_all(lambda op: [(0b111010 << 26) | (0x0A << 19) | ((op & 7) << 16), 1 | (2 << 8) | (1 << 16) | (((op >> 3) & 1) << 21) | (0x80 << 24)], 16)
# MIMG: 111100 op[24:18] dmask[11:8] dim[5:3] | vaddr vdata srsrc ssamp   (op bit7 at [0])
tables['MIMG'] = name_all(None, 256, [lambda op: [(0b111100 << 26) | ((op & 0x7F) << 18) | (0xF << 8) | ((op >> 7) & 1), 1 | (2 << 8) | (1 << 16) | (2 << 21)], lambda op: [(0b111100 << 26) | ((op & 0x7F) << 18) | (0xF << 8) | ((op >> 7) & 1), 1 | (2 << 8) | (1 << 16)], lambda op: [(0b111100 << 26) | ((op & 0x7F) << 18) | (0x1 << 8) | ((op >> 7) & 1), 1 | (2 << 8) | (1 << 16)]])
# FLAT/GLOBAL/SCRATCH: 110111 op[24:18] seg[15:14] | addr data saddr vdst
for seg, name in ((0, 'FLAT'), (2, 'GLOBAL'), (1, 'SCRATCH')):
    tables[name] = name_all(lambda op, seg=seg: [(0b110111 << 26) | (op << 18) | (seg << 14), 1 | (2 << 8) | (0x7D << 16) | (3 << 24)], 128)

json.dump({k: {str(o): n for o, n in v.items()} for k, v in tables.items()}, open('gfx10_ops.json', 'w'), indent=1)
for k, v in tables.items():
    print(k, len(v))
