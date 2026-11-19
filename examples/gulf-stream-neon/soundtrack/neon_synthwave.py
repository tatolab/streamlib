# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The reel's soundtrack, synthesised from scratch: 24 s of synthwave at 120 BPM.

Every sound is generated here (band-limited saws, a sine kick, filtered noise)
and arranged to the take: an intro swell, a groove from 2 s, a build from 8 s,
the drop on the downbeat at 10 s, a breakdown at 20 s and a tape stop. Writes
a 48 kHz stereo WAV.

    python neon_synthwave.py out.wav
"""

from __future__ import annotations

import sys

import numpy as np
from scipy import signal

RATE = 48_000
LENGTH_SECONDS = 24.0
BPM = 120.0
BEAT = 60.0 / BPM
BAR = 4 * BEAT
N = int(RATE * LENGTH_SECONDS)
T = np.arange(N) / RATE
RNG = np.random.default_rng(7)

DROP = 10.0
BUILD = 8.0
GROOVE = 2.0
BREAKDOWN = 20.0
TAPE_STOP = 22.4

# Am, F, C, G: one bar each.
CHORDS = [(57, 60, 64), (53, 57, 60), (55, 60, 64), (55, 59, 62)]
BASS_ROOTS = [33, 29, 36, 31]
LEAD = [
    [76, None, 76, 74, 72, None, 74, 76],
    [77, None, 76, None, 72, None, 69, None],
    [76, None, 79, None, 76, 74, 72, None],
    [74, None, 71, None, 74, None, 79, None],
]


def midi_hz(note: float) -> float:
    return 440.0 * 2.0 ** ((note - 69) / 12.0)


def chord_at(seconds: float) -> int:
    return int(seconds // BAR) % 4


def poly_blep(phase: np.ndarray, dt: np.ndarray) -> np.ndarray:
    out = np.zeros_like(phase)
    low = phase < dt
    x = phase[low] / dt[low]
    out[low] = x + x - x * x - 1.0
    high = phase > 1.0 - dt
    x = (phase[high] - 1.0) / dt[high]
    out[high] = x * x + x + x + 1.0
    return out


def saw(freq: np.ndarray, phase0: float = 0.0) -> np.ndarray:
    dt = freq / RATE
    phase = (np.cumsum(dt) + phase0) % 1.0
    return 2.0 * phase - 1.0 - poly_blep(phase, dt)


def lowpass_sweep(x: np.ndarray, cutoff: np.ndarray, q: float = 0.9, block: int = 256) -> np.ndarray:
    """A resonant low-pass whose cutoff moves block by block."""
    y = np.empty_like(x)
    zi = np.zeros((1, 2))
    for start in range(0, len(x), block):
        c = float(np.clip(cutoff[min(start, len(cutoff) - 1)], 40.0, RATE * 0.45))
        b, a = signal.iirfilter(2, c, btype="low", ftype="butter", fs=RATE)
        # A touch of resonance: sharpen the pole pair.
        a = a.copy()
        a[1] *= 1.0 - 0.04 * q
        sos = np.concatenate([b, a])[None, :]
        y[start:start + block], zi = signal.sosfilt(sos, x[start:start + block], zi=zi)
    return y


def band(x: np.ndarray, low: float, high: float) -> np.ndarray:
    sos = signal.butter(2, [low, high], btype="band", fs=RATE, output="sos")
    return signal.sosfilt(sos, x)


def highpass(x: np.ndarray, cutoff: float) -> np.ndarray:
    sos = signal.butter(2, cutoff, btype="high", fs=RATE, output="sos")
    return signal.sosfilt(sos, x)


def place(track: np.ndarray, sound: np.ndarray, at: float, gain: float = 1.0) -> None:
    start = int(at * RATE)
    if start >= len(track):
        return
    end = min(len(track), start + len(sound))
    track[start:end] += sound[: end - start] * gain


def envelope(length: float, attack: float, decay: float) -> np.ndarray:
    t = np.arange(int(length * RATE)) / RATE
    return np.minimum(1.0, t / max(attack, 1e-4)) * np.exp(-t / decay)


def kick() -> np.ndarray:
    t = np.arange(int(0.45 * RATE)) / RATE
    pitch = 44.0 + 120.0 * np.exp(-t / 0.035)
    body = np.sin(2 * np.pi * np.cumsum(pitch) / RATE) * np.exp(-t / 0.22)
    click = RNG.standard_normal(len(t)) * np.exp(-t / 0.003) * 0.3
    return np.tanh(1.6 * (body + click))


def snare() -> np.ndarray:
    t = np.arange(int(0.3 * RATE)) / RATE
    noise = band(RNG.standard_normal(len(t)), 900.0, 7000.0) * np.exp(-t / 0.11)
    tone = np.sin(2 * np.pi * 185.0 * t) * np.exp(-t / 0.06)
    return noise * 0.9 + tone * 0.6


def hat(open_hat: bool = False) -> np.ndarray:
    length = 0.25 if open_hat else 0.06
    t = np.arange(int(length * RATE)) / RATE
    return highpass(RNG.standard_normal(len(t)), 7500.0) * np.exp(-t / (0.09 if open_hat else 0.018))


def crash() -> np.ndarray:
    t = np.arange(int(2.5 * RATE)) / RATE
    return highpass(RNG.standard_normal(len(t)), 4000.0) * np.exp(-t / 0.9)


def reverb_ir(seconds: float, gated: bool = False) -> np.ndarray:
    t = np.arange(int(seconds * RATE)) / RATE
    tail = np.ones_like(t) if gated else np.exp(-t / (seconds / 4.0))
    ir = RNG.standard_normal((2, len(t))) * tail
    ir[:, : int(0.01 * RATE)] *= np.linspace(0, 1, int(0.01 * RATE))
    return ir / np.sqrt(np.sum(ir[0] ** 2))


def stereo_reverb(x: np.ndarray, ir: np.ndarray) -> np.ndarray:
    return np.stack([signal.fftconvolve(x, ir[0])[:N], signal.fftconvolve(x, ir[1])[:N]])


def kick_times() -> list[float]:
    times = []
    beat_time = GROOVE
    while beat_time < BREAKDOWN:
        if not (BUILD + 1.5 <= beat_time < DROP):
            times.append(beat_time)
        beat_time += BEAT
    return times


def sidechain(kicks: list[float], depth: float = 0.75) -> np.ndarray:
    gain = np.ones(N)
    for k in kicks:
        start = int(k * RATE)
        t = np.arange(min(N - start, int(0.45 * RATE))) / RATE
        gain[start:start + len(t)] = np.minimum(gain[start:start + len(t)], 1.0 - depth * np.exp(-t / 0.13))
    return gain


def note_track(events: list[tuple[float, float, float]], voice) -> np.ndarray:
    """Renders (start, duration, midi) events with `voice(freq_array, seconds)`."""
    out = np.zeros(N)
    for start, duration, note in events:
        length = int((duration + 0.25) * RATE)
        s = int(start * RATE)
        if s >= N:
            continue
        length = min(length, N - s)
        freq = np.full(length, midi_hz(note))
        sound = voice(freq, duration)
        out[s:s + length] += sound[:length]
    return out


def pluck_voice(freq: np.ndarray, duration: float) -> np.ndarray:
    t = np.arange(len(freq)) / RATE
    tone = saw(freq) + 0.5 * saw(freq * 1.004, 0.3)
    env = np.minimum(1.0, t / 0.004) * np.exp(-t / 0.16)
    return tone * env


def lead_voice(freq: np.ndarray, duration: float) -> np.ndarray:
    t = np.arange(len(freq)) / RATE
    vibrato = 1.0 + 0.004 * np.sin(2 * np.pi * 5.5 * t) * np.minimum(1.0, t / 0.25)
    tone = saw(freq * vibrato) + saw(freq * vibrato * 1.006, 0.5) + 0.4 * saw(freq * 2.0 * vibrato, 0.2)
    gate = np.clip((duration - t) / 0.06 + 1.0, 0.0, 1.0)
    env = np.minimum(1.0, t / 0.01) * (0.75 + 0.25 * np.exp(-t / 0.2)) * gate
    return tone * env


def supersaw(freqs: list[float], length: int) -> np.ndarray:
    out = np.zeros((2, length))
    for note_hz in freqs:
        for i, detune in enumerate([-0.011, -0.005, 0.0, 0.006, 0.012]):
            wave = saw(np.full(length, note_hz * (1.0 + detune)), RNG.random())
            pan = (i / 4.0)
            out[0] += wave * (1.0 - pan)
            out[1] += wave * pan
    return out


def main(path: str) -> None:
    left = np.zeros(N)
    right = np.zeros(N)
    reverb_send = np.zeros(N)

    # --- Pad: chords the whole way, swelling in, filter opening into the drop.
    pad_cutoff = np.interp(T, [0, 2, 8, 9.9, 10, 20, 22, 24], [300, 1200, 1500, 5200, 2600, 3000, 1800, 600])
    pad_gain = np.interp(T, [0, 1.8, 2, 10, 20, 22.4, 24], [0.0, 0.5, 0.42, 0.45, 0.6, 0.55, 0.0])
    pad = np.zeros((2, N))
    for bar in range(int(LENGTH_SECONDS / BAR) + 1):
        start = int(bar * BAR * RATE)
        if start >= N:
            break
        length = min(N - start, int(BAR * RATE) + int(0.05 * RATE))
        notes = [midi_hz(n) for n in CHORDS[bar % 4]] + [midi_hz(CHORDS[bar % 4][0] - 12)]
        chunk = supersaw(notes, length) * 0.08
        fade = np.minimum(1.0, np.minimum(np.arange(length), length - np.arange(length)) / (0.03 * RATE))
        end = start + length
        pad[:, start:end] += chunk * fade
    pad[0] = lowpass_sweep(pad[0], pad_cutoff)
    pad[1] = lowpass_sweep(pad[1], pad_cutoff)
    pad *= pad_gain

    kicks = kick_times()
    duck = sidechain(kicks)

    # --- Bass: 8th-note octave pulse from the groove, a fat saw in the drop.
    bass_events = []
    t = GROOVE
    while t < TAPE_STOP:
        if not (BUILD + 1.75 <= t < DROP):
            root = BASS_ROOTS[chord_at(t)]
            octave = 12 if int(round((t - GROOVE) / (BEAT / 2))) % 2 else 0
            bass_events.append((t, BEAT / 2 * 0.9, root + octave))
        t += BEAT / 2

    def bass_voice(freq: np.ndarray, duration: float) -> np.ndarray:
        tt = np.arange(len(freq)) / RATE
        tone = saw(freq) + saw(freq * 1.003, 0.4) + 0.8 * np.sin(2 * np.pi * np.cumsum(freq) / RATE)
        gate = np.clip((duration - tt) / 0.01 + 1.0, 0.0, 1.0)
        return tone * np.minimum(1.0, tt / 0.003) * gate

    bass = note_track(bass_events, bass_voice)
    bass_cutoff = np.interp(T, [2, 8, 9.7, 10, 20, 22.4], [400, 900, 2400, 1400, 1700, 500])
    bass = lowpass_sweep(bass, bass_cutoff) * np.interp(T, [0, 2, 10, 20, 22.4, 24], [0, 0.32, 0.42, 0.4, 0.3, 0]) * duck

    # --- Arp: 16th-note plucks through the chord, ping-pong delayed.
    arp_events = []
    t = GROOVE
    step = 0
    while t < TAPE_STOP:
        chord = CHORDS[chord_at(t)]
        pattern = [chord[0] + 12, chord[1] + 12, chord[2] + 12, chord[1] + 12, chord[0] + 24, chord[2] + 12, chord[1] + 12, chord[2] + 12]
        arp_events.append((t, BEAT / 4, pattern[step % 8]))
        t += BEAT / 4
        step += 1
    arp = note_track(arp_events, pluck_voice)
    arp_cutoff = np.interp(T, [2, 8, 9.9, 10, 20, 24], [1500, 2500, 7000, 4000, 4500, 2000])
    arp = lowpass_sweep(arp, arp_cutoff) * np.interp(T, [0, 2, 9.9, 10, 20, 22.4, 24], [0, 0.10, 0.16, 0.11, 0.14, 0.12, 0]) * (0.5 + 0.5 * duck)
    delay = int(BEAT * 0.75 * RATE)
    arp_left = arp.copy()
    arp_right = arp * 0.6
    arp_right[delay:] += arp[:-delay] * 0.55
    arp_left[2 * delay:] += arp[:-2 * delay] * 0.35

    # --- Lead: the hook, in the drop only.
    lead_events = []
    for bar in range(5):
        bar_start = DROP + bar * BAR
        pattern = LEAD[bar % 4]
        for i, note in enumerate(pattern):
            if note is None:
                continue
            length = 1
            while i + length < 8 and pattern[i + length] is None:
                length += 1
            lead_events.append((bar_start + i * BEAT / 2, length * BEAT / 2 * 0.95, note))
    lead = note_track(lead_events, lead_voice)
    lead = lowpass_sweep(lead, np.full(N, 5200.0)) * 0.085

    # --- Drums.
    drums = np.zeros(N)
    snare_bus = np.zeros(N)
    kick_sound = kick()
    for k in kicks:
        place(drums, kick_sound, k, 0.72 if k >= DROP else 0.55)
    place(drums, kick_sound, DROP, 0.4)
    # Sub boom on the drop.
    tt = np.arange(int(1.6 * RATE)) / RATE
    place(drums, np.sin(2 * np.pi * 36.0 * tt) * np.exp(-tt / 0.6), DROP, 0.9)

    snare_sound = snare()
    beat_time = GROOVE + BEAT
    while beat_time < BUILD:
        if beat_time >= GROOVE + BAR:
            place(snare_bus, snare_sound, beat_time, 0.45)
        beat_time += 2 * BEAT
    # The roll: 8ths, 16ths, then 32nds, rising.
    roll_time = BUILD
    while roll_time < DROP - 0.25:
        progress = (roll_time - BUILD) / (DROP - BUILD)
        division = 2 if progress < 0.4 else (4 if progress < 0.75 else 8)
        place(snare_bus, snare_sound, roll_time, 0.15 + 0.45 * progress)
        roll_time += BEAT / division
    beat_time = DROP + BEAT
    while beat_time < BREAKDOWN:
        place(snare_bus, snare_sound, beat_time, 0.8)
        beat_time += 2 * BEAT

    hats = np.zeros(N)
    hat_time = GROOVE + BEAT / 2
    while hat_time < BREAKDOWN:
        if not (BUILD + 1.5 <= hat_time < DROP):
            place(hats, hat(open_hat=hat_time >= DROP), hat_time, 0.16 if hat_time >= DROP else 0.12)
        hat_time += BEAT
    if True:
        sixteenth = DROP
        while sixteenth < BREAKDOWN:
            place(hats, hat(), sixteenth + BEAT / 4, 0.06)
            sixteenth += BEAT / 2

    crash_sound = crash()
    for at in (GROOVE, DROP, DROP + 4 * BAR, BREAKDOWN):
        place(hats, crash_sound, at, 0.22)

    # --- FX: the opening whoosh and the build's riser.
    whoosh_t = np.arange(int(2.0 * RATE)) / RATE
    whoosh_noise = RNG.standard_normal(len(whoosh_t))
    whoosh = lowpass_sweep(whoosh_noise, 300 + 9000 * (whoosh_t / 2.0) ** 2) * (whoosh_t / 2.0) ** 2 * 0.25
    place(left, whoosh, 0.0, 1.0)
    place(right, whoosh, 0.0, 1.0)
    riser_t = np.arange(int(2.0 * RATE)) / RATE
    riser = lowpass_sweep(RNG.standard_normal(len(riser_t)), 400 + 12000 * (riser_t / 2.0) ** 3) * (riser_t / 2.0) ** 2 * 0.3
    riser_tone = np.sin(2 * np.pi * np.cumsum(200 + 1400 * (riser_t / 2.0) ** 2) / RATE) * (riser_t / 2.0) ** 3 * 0.05
    place(left, riser + riser_tone, BUILD, 1.0)
    place(right, riser + riser_tone, BUILD, 1.0)

    # --- Mix.
    reverb_send += lead * 0.6 + arp * 0.4 + (pad[0] + pad[1]) * 0.25
    hall = stereo_reverb(reverb_send, reverb_ir(2.2)) * 0.35
    gated = stereo_reverb(snare_bus, reverb_ir(0.28, gated=True)) * 0.45

    left += pad[0] + bass + arp_left + lead * 0.9 + drums + snare_bus + hats * 0.9 + hall[0] + gated[0]
    right += pad[1] + bass + arp_right + lead * 1.0 + drums + snare_bus + hats * 1.1 + hall[1] + gated[1]
    mix = np.stack([left, right])

    # --- Tape stop: playback slows to a halt from TAPE_STOP.
    stop_length = 1.1
    out_t = T.copy()
    in_stop = T >= TAPE_STOP
    local = np.minimum(T[in_stop] - TAPE_STOP, stop_length)
    out_t[in_stop] = TAPE_STOP + local - local ** 2 / (2.0 * stop_length)
    mix = np.stack([np.interp(out_t, T, mix[0]), np.interp(out_t, T, mix[1])])
    fade = np.interp(T, [0, 0.02, TAPE_STOP + 0.9, TAPE_STOP + stop_length, LENGTH_SECONDS], [0, 1, 1, 0, 0])
    mix *= fade

    mix = np.tanh(mix * 1.3) / np.tanh(1.3)
    mix /= np.max(np.abs(mix)) * 1.06
    pcm = (mix.T * 32767).astype("<i2")

    import wave

    with wave.open(path, "wb") as out:
        out.setnchannels(2)
        out.setsampwidth(2)
        out.setframerate(RATE)
        out.writeframes(pcm.tobytes())


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "soundtrack.wav")
