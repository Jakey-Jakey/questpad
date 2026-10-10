"""funcs.py <file.c> <regex> [--print] : list (or print) decompiled functions matching regex."""
import re, sys
c = open(sys.argv[1]).read()
pat = re.compile(sys.argv[2])
for f in re.split(r'^// ===== ', c, flags=re.M)[1:]:
    if pat.search(f):
        if '--print' in sys.argv: print('// =====', f)
        else: print(f.split('\n', 1)[0], ' lines:', f.count('\n'))
