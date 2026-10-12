# SPDX-License-Identifier: AGPL-3.0-or-later
"""Transcribe WAVs with faster-whisper and report word error rate against the source text.

Usage: uv run asr.py TEXT WAV [WAV ...] [--model small.en]
Text is normalised (lowercase, punctuation removed, whitespace collapsed) on both sides.
"""
import argparse, json, re

import jiwer
import numpy as np
import soundfile as sf
from faster_whisper import WhisperModel

ap = argparse.ArgumentParser()
ap.add_argument("text")
ap.add_argument("wavs", nargs="+")
ap.add_argument("--model", default="small.en")
a = ap.parse_args()


def norm(s: str) -> str:
    s = s.lower().replace("—", " ").replace("–", " ").replace("-", " ")
    s = re.sub(r"[^\w\s%£$]", " ", s)
    return " ".join(s.split())


ref = norm(open(a.text, encoding="utf-8").read())
model = WhisperModel(a.model, device="cpu", compute_type="int8")
for wav in a.wavs:
    audio, rate = sf.read(wav, dtype="float32")
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    # Resample to 16 kHz for Whisper (linear interpolation is fine for speech recognition).
    n = int(len(audio) * 16000 / rate)
    audio = np.interp(np.linspace(0, len(audio) - 1, n), np.arange(len(audio)), audio).astype(np.float32)
    segments, _ = model.transcribe(audio, beam_size=5, language="en")
    hyp = norm(" ".join(seg.text for seg in segments))
    out = jiwer.process_words(ref, hyp)
    print(json.dumps({
        "wav": wav, "asr_model": a.model, "wer": round(out.wer, 4),
        "substitutions": out.substitutions, "deletions": out.deletions, "insertions": out.insertions,
        "ref_words": len(ref.split()),
    }))
    with open(wav + ".transcript.txt", "w", encoding="utf-8") as f:
        f.write(hyp + "\n")
