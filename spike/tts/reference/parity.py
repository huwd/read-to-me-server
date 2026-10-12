# SPDX-License-Identifier: AGPL-3.0-or-later
"""Word-level phoneme parity between the Rust spike and Python Misaki.
Usage: python3 parity.py RUST.phon PYTHON.phon [N]   (N = how many top differences to list)"""
import collections, difflib, sys

rust = open(sys.argv[1], encoding="utf-8").read().split("\n")
py = open(sys.argv[2], encoding="utf-8").read().split("\n")
top = int(sys.argv[3]) if len(sys.argv) > 3 else 20
total = same = 0
diffs = collections.Counter()
for a, b in zip(rust, py):
    aw, bw = a.split(), b.split()
    for op, i1, i2, j1, j2 in difflib.SequenceMatcher(None, aw, bw, autojunk=False).get_opcodes():
        if op == "equal":
            same += i2 - i1
        else:
            diffs[(" ".join(aw[i1:i2]), " ".join(bw[j1:j2]))] += 1
    total += len(bw)
print(f"python words {total}, identical in rust: {same} ({100 * same / total:.1f}%)")
for (r, p), n in diffs.most_common(top):
    print(f"{n:3}  rust {r!r:40} python {p!r}")
