# SPDX-License-Identifier: AGPL-3.0-or-later
"""Print Misaki (Python) phonemes per paragraph, applying the same overrides and quote
normalisation as the Rust spike, for a parity check. Usage: uv run phonemes.py TEXT [--us]"""
import re, sys
from misaki import en, espeak

british = "--us" not in sys.argv
text = open(sys.argv[1], encoding="utf-8").read()
text = text.translate(str.maketrans({"‘": "'", "’": "'", "“": '"', "”": '"', " ": " ", "​": None}))
for line in open("../pronunciations.tsv", encoding="utf-8"):
    if line.strip() and not line.startswith("#"):
        word, ph = line.rstrip("\n").split("\t")
        text = re.sub(rf"(?<!\w){re.escape(word)}(?!\w)", f"[{word}](/{ph}/)", text)
fallback = espeak.EspeakFallback(british=british)
g2p = en.G2P(trf=False, british=british, fallback=fallback)
for para in (p.strip() for p in text.split("\n")):
    if para:
        print(g2p(para)[0])
