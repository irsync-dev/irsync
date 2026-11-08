#!/usr/bin/env python3
"""Count syscalls per irsync call site in an `strace -f -k` trace.

Usage: tools/syscall-sites.py TRACE BINARY [FRAMES]

BINARY is the traced irsync, built with line tables (see
book/src/contributing/performance.md, "Syscalls per call site"). Each output line is a count, the syscall (with
"(fd)" for a stat through a descriptor and the errno of failed calls) and
the innermost FRAMES (default 3) frames in irsync's own sources."""
import collections, re, subprocess, sys

trace, binary = sys.argv[1], sys.argv[2]
depth = int(sys.argv[3]) if len(sys.argv) > 3 else 3
name = re.escape(binary.rsplit("/", 1)[-1])

# strace prints file offsets; addr2line wants virtual addresses. The
# difference is the executable segment's vaddr minus its file offset.
for l in subprocess.run(["readelf", "-lW", binary], capture_output=True, text=True).stdout.splitlines():
    f = l.split()
    if f[:1] == ["LOAD"] and "E" in f[6:-1]:
        shift = int(f[2], 16) - int(f[1], 16)

calls, pending, cur = [], {}, None
for line in open(trace):
    if line.startswith(" > "):
        m = re.search(name + r"\(\S*\) \[(0x[0-9a-f]+)\]", line)
        if cur is not None and m:
            # A return address points after the call: step back into it.
            cur[1].append(int(m.group(1), 16) + shift - 1)
        continue
    cur = None
    m = re.match(r"(\d+) +<\.\.\. (\w+) resumed>(.*)", line)
    if m:  # an interleaved call: its stack follows the resumed line
        cur = pending.pop(m.group(1), None)
        err = re.search(r"= -1 (E\w+)", m.group(3))
        if cur is not None and err:
            cur[0] += " " + err.group(1)
        continue
    m = re.match(r"(\d+) +(\w+)\((.*)", line)
    if m:
        pid, sc, rest = m.groups()
        if "AT_EMPTY_PATH" in rest:
            sc += " (fd)"
        cur = [sc, []]
        calls.append(cur)
        if rest.endswith("<unfinished ...>"):
            pending[pid], cur = cur, None
        elif err := re.search(r"= -1 (E\w+)", rest):
            cur[0] += " " + err.group(1)

addrs = sorted({a for c in calls for a in c[1]})
out = subprocess.run(["addr2line", "-a", "-f", "-C", "-i", "-e", binary] + [hex(a) for a in addrs],
                     capture_output=True, text=True).stdout.splitlines()
frames, a = {}, None
i = 0
while i < len(out):
    if re.fullmatch(r"0x[0-9a-f]+", out[i]):
        a = int(out[i], 16)
        frames[a] = []
        i += 1
    else:  # function, then file:line
        frames[a].append((out[i], out[i + 1] if i + 1 < len(out) else ""))
        i += 2

def site(stack):
    keep = []
    for addr in stack:
        for fn, loc in frames.get(addr, []):
            if "/irsync/src/" in loc and "/fs/transient" not in loc:
                fn = re.sub(r"<.*?>", "", fn).split("::")[-1]
                keep.append(f"{fn} ({loc.split('/src/')[-1].split()[0]})")
    return " < ".join(keep[:depth]) or "(no irsync frame)"

counts = collections.Counter((c[0], site(c[1])) for c in calls)
for (sc, where), n in counts.most_common():
    print(f"{n:8d}  {sc:24s} {where}")
