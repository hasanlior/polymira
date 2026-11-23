import glob, re, subprocess, sys
ORBIS = r'C:/Users/Casper/OneDrive/Masaüstü/Projeler/port/emu/orbis/target/release/orbis.exe'
base = lambda m: re.sub(r'_(e32|e64|sdwa|dpp|dpp8)$', '', m)
total = bad = 0
for s in sorted(glob.glob('dis/*.s')):
    llvm = []
    for line in open(s):
        m = re.match(r'\t(\S+).*//\s*([0-9A-F]+):((?: [0-9A-F]{8})+)', line)
        if m:
            llvm.append((int(m.group(2), 16), len(m.group(3).split()), m.group(1)))
            if m.group(1) == 's_endpgm':
                break
    bin_ = 'shaders/' + __import__('os').path.basename(s)[:-2] + '.bin'
    out = subprocess.run([ORBIS, 'rdna-dis', bin_], capture_output=True, text=True).stdout.split('\n')
    ours = [(int(a, 16), int(b), c) for a, b, c in (l.split() for l in out if l.strip())]
    for i, (L, O) in enumerate(zip(llvm, ours)):
        total += 1
        if L[0] != O[0] or L[1] != O[1] or base(L[2]) != base(O[2]):
            bad += 1
            if bad <= 15:
                print(s, 'LLVM', L, 'ORBIS', O)
            break
    if len(llvm) != len(ours):
        print(s, 'count differs', len(llvm), len(ours))
print('checked', total, 'mismatches', bad)
