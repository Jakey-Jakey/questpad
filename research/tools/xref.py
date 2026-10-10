"""xref.py <bin> <base> <text> : find string, literal-pool refs, and functions in decompiled C that use them."""
import re, struct, sys
path, base, text = sys.argv[1], int(sys.argv[2], 0), sys.argv[3].encode()
d = open(path, 'rb').read()
cfile = sys.argv[4] if len(sys.argv) > 4 else None
for m in re.finditer(re.escape(text), d):
    # walk back to string start
    s = m.start()
    while s > 0 and 0x20 <= d[s-1] < 0x7f: s -= 1
    addr = base + s
    refs = [base + i for i in range(0, len(d) - 3, 4) if struct.unpack_from('<I', d, i)[0] == addr]
    full = d[s:d.index(b'\0', s)].decode(errors='replace')
    print(f'string @ {addr:#x}: {full!r}  pool refs: {[hex(r) for r in refs]}')
    if cfile:
        c = open(cfile).read()
        funcs = re.split(r'^// ===== ', c, flags=re.M)
        pat = re.compile(r'0x%x\b' % addr, re.I)
        for f in funcs:
            if pat.search(f) or ('DAT_%08x' % addr) in f or ('s_' in f and ('_%08x' % addr) in f):
                print('   used in', f.split('\n', 1)[0])
