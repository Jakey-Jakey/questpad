import re, struct, sys
from collections import Counter
path, skip = sys.argv[1], int(sys.argv[2], 0) if len(sys.argv) > 2 else 0
d = open(path, 'rb').read()[skip:]
strs = [m.start() for m in re.finditer(rb'[\x20-\x7e]{8,}\x00', d)]
words = Counter(struct.unpack_from('<I', d, i)[0] for i in range(0, len(d) - 3, 4))
best = []
for base in range(0, 0x80000, 0x100):
    hits = sum(1 for s in strs if words.get(base + s))
    best.append((hits, base))
best.sort(reverse=True)
print(path, 'strings', len(strs), 'top bases:', [(hex(b), h) for h, b in best[:5]])
