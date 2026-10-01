#!/usr/bin/env python3
"""Regenerate Hydra's short press/release WAV cues using only Python's stdlib."""
import math
from pathlib import Path
import struct
import wave

assets = Path(__file__).resolve().parent.parent / 'assets'
assets.mkdir(exist_ok=True)
rate = 48000
for name, frequency, decay in [('press', 1500, 38), ('release', 900, 30)]:
    samples = []
    for index in range(int(rate * 0.14)):
        t = index / rate
        envelope = min(t * 5000, 1) * math.exp(-t * decay)
        tone = 0.8 * math.sin(math.tau * frequency * t)
        impact = 0.2 * math.sin(math.tau * 3700 * t)
        samples.append(struct.pack('<h', int(0.65 * 32767 * envelope * (tone + impact))))
    with wave.open(str(assets / f'{name}.wav'), 'wb') as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(b''.join(samples))
