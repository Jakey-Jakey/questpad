"""thumbdis.py <bin> <base> <start> <end> : thumb disassembly with literal-pool values annotated."""
import struct, sys
from capstone import Cs, CS_ARCH_ARM, CS_MODE_THUMB
path, base, start, end = sys.argv[1], int(sys.argv[2], 0), int(sys.argv[3], 0), int(sys.argv[4], 0)
d = open(path, 'rb').read()
md = Cs(CS_ARCH_ARM, CS_MODE_THUMB)
md.detail = False
for ins in md.disasm(d[start - base:end - base], start):
    note = ''
    if ins.mnemonic.startswith('ldr') and '[pc' in ins.op_str:
        try:
            imm = int(ins.op_str.split('#')[-1].rstrip(']'), 0)
            a = ((ins.address + 4) & ~3) + imm
            note = f'   ; [{a:#x}] = {struct.unpack_from("<I", d, a - base)[0]:#010x}'
        except Exception:
            pass
    print(f'{ins.address:08x}  {ins.mnemonic:8s} {ins.op_str}{note}')
