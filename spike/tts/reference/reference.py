# SPDX-License-Identifier: AGPL-3.0-or-later
"""Synthesise a text file with the Python Kokoro reference (hexgrad/kokoro), for comparison.

Usage: uv run reference.py TEXT OUT.wav [--voice bf_emma] [--threads N]
Prints one JSON line of stats, like the Rust spike.
"""
import argparse, json, resource, time

import numpy as np
import soundfile as sf
import torch
from kokoro import KPipeline

ap = argparse.ArgumentParser()
ap.add_argument("text")
ap.add_argument("out")
ap.add_argument("--voice", default="bf_emma")
ap.add_argument("--threads", type=int, default=torch.get_num_threads())
a = ap.parse_args()
torch.set_num_threads(a.threads)

t0 = time.perf_counter()
pipe = KPipeline(lang_code="b" if a.voice.startswith("b") else "a", repo_id="hexgrad/Kokoro-82M")
load_s = time.perf_counter() - t0

text = open(a.text, encoding="utf-8").read()
paragraphs = [p.strip() for p in text.split("\n") if p.strip()]
gap = np.zeros(int(24000 * 0.5), dtype=np.float32)
audio = []
t1 = time.perf_counter()
for i, para in enumerate(paragraphs):
    for result in pipe(para, voice=a.voice, speed=1.0):
        if result.audio is not None:
            audio.append(result.audio.numpy())
    if i + 1 < len(paragraphs):
        audio.append(gap)
synth_s = time.perf_counter() - t1
wave = np.concatenate(audio) if audio else np.zeros(1, dtype=np.float32)
sf.write(a.out, wave, 24000, subtype="PCM_16")
audio_s = len(wave) / 24000
print(json.dumps({
    "engine": "python-kokoro", "text": a.text, "voice": a.voice, "threads": a.threads,
    "words": len(text.split()), "load_s": round(load_s, 2), "audio_s": round(audio_s, 1),
    "synth_s": round(synth_s, 1), "rtf": round(audio_s / synth_s, 2),
    "peak_rss_mb": round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024),
}))
