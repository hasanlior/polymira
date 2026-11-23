import struct, sys, glob, os
def elf(code):
    shstr = b'\0.text\0.shstrtab\0'
    text_off = 64
    shstr_off = text_off + len(code)
    sh_off = (shstr_off + len(shstr) + 7) & ~7
    eh = b'\x7fELF' + bytes([2, 1, 1, 64, 3]) + b'\0' * 7
    eh += struct.pack('<HHIQQQIHHHHHH', 1, 224, 1, 0, 0, sh_off, 0x36, 64, 0, 0, 64, 3, 2)
    out = bytearray(eh) + code + shstr
    out += b'\0' * (sh_off - len(out))
    out += b'\0' * 64
    out += struct.pack('<IIQQQQIIQQ', 1, 1, 6, 0, text_off, len(code), 0, 0, 256, 0)
    out += struct.pack('<IIQQQQIIQQ', 7, 3, 0, 0, shstr_off, len(shstr), 0, 0, 1, 0)
    return bytes(out)
for f in sorted(glob.glob('shaders/*.bin')):
    open(f[:-4] + '.o', 'wb').write(elf(open(f, 'rb').read()))
