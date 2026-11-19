# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""ElevenLabs take 3 for the reel: lift the quiet intro with gain automation (no compression),
keep the drop at 16.0 s hitting, trim to 28.0 s, and normalise to -14 LUFS with true peak <= -1 dBTP."""
import sys
import numpy as np
import soundfile as sf
import librosa
import pyloudnorm as pyln

src, out = sys.argv[1], sys.argv[2]
y, sr = librosa.load(src, sr=48000, mono=False)
y = y.T.astype(np.float64)
n = int(28.0 * sr)
y = y[:n]
t = np.arange(len(y)) / sr
# Gain automation in dB: +8 dB over the intro, easing to +3 dB into the build, 0 dB from the drop.
lift = np.interp(t, [0.0, 12.0, 15.0, 15.98, 16.0, 28.0], [8.0, 7.0, 3.0, 3.0, 0.0, 0.0])
y = y * (10 ** (lift / 20))[:, None]
meter = pyln.Meter(sr)
loud = meter.integrated_loudness(y)
y = y * 10 ** ((-14.0 - loud) / 20)
# A gentle look-ahead peak ceiling at -1 dBTP (4x oversampled estimate), only where it is needed.
over = librosa.resample(y.T, orig_sr=sr, target_sr=sr * 4).T
peak = np.max(np.abs(over))
ceiling = 10 ** (-1.0 / 20)
if peak > ceiling:
    env = np.max(np.abs(y), axis=1)
    win = int(0.005 * sr)
    env = np.maximum.accumulate(np.lib.stride_tricks.sliding_window_view(np.pad(env, (0, win - 1), mode="edge"), win).max(axis=1)[None, :], axis=0)[0]
    gain = np.minimum(1.0, ceiling * 0.97 / np.maximum(env, 1e-9))
    smooth = np.convolve(gain, np.ones(win) / win, mode="same")
    y = y * np.minimum(gain, smooth)[:, None]
# 20 ms fade at the very end only.
fade = int(0.02 * sr)
y[-fade:] *= np.linspace(1, 0, fade)[:, None]
sf.write(out, y.astype(np.float32), sr, subtype="PCM_24")
y2, _ = sf.read(out)
print("LUFS", round(meter.integrated_loudness(y2), 2), "first-second RMS dBFS", round(20 * np.log10(np.sqrt(np.mean(y2[:sr] ** 2))), 1), "body RMS", round(20 * np.log10(np.sqrt(np.mean(y2[16 * sr:24 * sr] ** 2))), 1), "peak dBFS", round(20 * np.log10(np.max(np.abs(y2))), 2))
