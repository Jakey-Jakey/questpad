"""filemap.py <file.c> : map decompiled functions to source files via referenced *.c strings."""
import re, sys
from collections import defaultdict
c = open(sys.argv[1]).read()
by_file = defaultdict(list)
for f in re.split(r'^// ===== ', c, flags=re.M)[1:]:
    head = f.split('\n', 1)[0]
    files = set(re.findall(r'PTR_s_([A-Za-z0-9_]+?)_c_[0-9a-f]{8}', f))
    for fn in files:
        by_file[fn].append(head.split(' @ ')[1])
for fn in sorted(by_file):
    addrs = sorted(set(by_file[fn]))
    print(f'{fn+".c":32s} {len(addrs):3d}  {addrs[0]}..{addrs[-1]}')
