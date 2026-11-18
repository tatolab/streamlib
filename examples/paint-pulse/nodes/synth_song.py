# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SynthSong`: an original 32-second dance loop, rendered once in numpy and streamed on a loop.

It stands in for a music file player, which the runtime does not have yet. The loop has an
intro, a build, a drop, a break and a second drop, so every part of the spectrum gets its turn.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

import numpy
from tatolab.stream import Channels, Output, RuntimeContextLimitedAccess, log, node

SAMPLE_RATE = 48_000
CHANNELS = 2
BLOCK_SIZE = 512
NANOSECONDS_PER_SECOND = 1_000_000_000
MAXIMUM_BLOCKS_OF_CATCH_UP = 8

BEATS_PER_MINUTE = 120.0
SECONDS_PER_BEAT = 60.0 / BEATS_PER_MINUTE
BEATS_PER_BAR = 4
BARS = 16
SIXTEENTHS_PER_BAR = 16
SECONDS_PER_SIXTEENTH = SECONDS_PER_BEAT / 4

# A minor, F major, C major, G major: one chord a bar.
CHORDS = ((57, 60, 64), (53, 57, 60), (48, 52, 55), (55, 59, 62))
BASS_ROOTS = (33, 29, 36, 31)


def midi_to_hz(note: float) -> float:
    return 440.0 * 2.0 ** ((note - 69.0) / 12.0)


def section_of_bar(bar: int) -> str:
    if bar < 4:
        return "intro"
    if bar < 8:
        return "build"
    if bar < 12:
        return "drop"
    if bar < 14:
        return "break"
    return "drop"


class LoopRenderer:
    """Adds note events into one stereo buffer, sample-accurate."""

    def __init__(self, seconds: float, seed: int = 11) -> None:
        self.buffer = numpy.zeros((CHANNELS, int(seconds * SAMPLE_RATE)), dtype=numpy.float64)
        self.random = numpy.random.default_rng(seed)

    def add(self, start_s: float, mono: numpy.ndarray, pan: float = 0.0) -> None:
        start = int(start_s * SAMPLE_RATE)
        end = min(self.buffer.shape[1], start + mono.shape[0])
        if end <= start:
            return
        left = math.cos((pan + 1.0) * math.pi / 4.0)
        right = math.sin((pan + 1.0) * math.pi / 4.0)
        self.buffer[0, start:end] += mono[: end - start] * left
        self.buffer[1, start:end] += mono[: end - start] * right

    # ---- instruments ----

    @staticmethod
    def time_axis(seconds: float) -> numpy.ndarray:
        return numpy.arange(int(seconds * SAMPLE_RATE)) / SAMPLE_RATE

    def kick(self) -> numpy.ndarray:
        t = self.time_axis(0.45)
        pitch = 45.0 + 110.0 * numpy.exp(-t * 28.0)
        phase = 2.0 * math.pi * numpy.cumsum(pitch) / SAMPLE_RATE
        return numpy.sin(phase) * numpy.exp(-t * 7.5) * 0.95

    def noise_hit(self, seconds: float, decay: float, brightness: float) -> numpy.ndarray:
        t = self.time_axis(seconds)
        white = self.random.standard_normal(t.shape[0])
        # A first difference tilts white noise towards the top; `brightness` blends it in.
        bright = numpy.diff(white, prepend=0.0)
        return (white * (1.0 - brightness) + bright * brightness) * numpy.exp(-t * decay)

    def hat(self, level: float) -> numpy.ndarray:
        return self.noise_hit(0.06, 70.0, 1.0) * level

    def clap(self) -> numpy.ndarray:
        t = self.time_axis(0.22)
        body = self.noise_hit(0.22, 18.0, 0.55) * 0.5
        tone = numpy.sin(2.0 * math.pi * 185.0 * t) * numpy.exp(-t * 30.0) * 0.25
        return body + tone

    def additive(
        self, hz: float, seconds: float, harmonics: int, tilt: float, attack: float, decay: float, detune_cents: float = 0.0
    ) -> numpy.ndarray:
        t = self.time_axis(seconds)
        voice = numpy.zeros_like(t)
        for detune in (-detune_cents, detune_cents) if detune_cents else (0.0,):
            f = hz * 2.0 ** (detune / 1200.0)
            for n in range(1, harmonics + 1):
                if f * n > SAMPLE_RATE * 0.45:
                    break
                voice += numpy.sin(2.0 * math.pi * f * n * t + n * 0.7) / (n**tilt)
        envelope = numpy.minimum(1.0, t / max(attack, 1e-4)) * numpy.exp(-t * decay)
        tail = numpy.minimum(1.0, (seconds - t) / 0.02)
        return voice * envelope * tail

    def pad(self, note: int, seconds: float, brightness: int) -> numpy.ndarray:
        return self.additive(midi_to_hz(note), seconds, brightness, 1.0, 0.5, 0.15, detune_cents=9.0) * 0.035

    def pluck(self, note: int, level: float) -> numpy.ndarray:
        return self.additive(midi_to_hz(note), 0.25, 6, 1.6, 0.003, 14.0) * level

    def bass(self, note: int, seconds: float) -> numpy.ndarray:
        return self.additive(midi_to_hz(note), seconds, 10, 1.1, 0.004, 6.0) * 0.22

    def sub(self, note: int, seconds: float) -> numpy.ndarray:
        return self.additive(midi_to_hz(note), seconds, 1, 1.0, 0.03, 1.2) * 0.45

    def lead(self, note: int) -> numpy.ndarray:
        return self.additive(midi_to_hz(note), 0.32, 7, 1.3, 0.01, 6.0, detune_cents=6.0) * 0.06

    def riser(self, seconds: float) -> numpy.ndarray:
        t = self.time_axis(seconds)
        noise = self.noise_hit(seconds, 0.0, 0.3)
        return noise * (t / seconds) ** 2 * 0.18


def render_the_loop() -> numpy.ndarray:
    """The whole loop, `[channel][sample]` float32, peaking just under full scale."""
    seconds_per_bar = SECONDS_PER_BEAT * BEATS_PER_BAR
    loop = LoopRenderer(BARS * seconds_per_bar)
    kick = loop.kick()
    clap = loop.clap()
    for bar in range(BARS):
        section = section_of_bar(bar)
        bar_start = bar * seconds_per_bar
        chord = CHORDS[bar % 4]
        root = BASS_ROOTS[bar % 4]
        build_rise = (bar - 4) / 4.0 if section == "build" else 0.0

        brightness = {"intro": 3, "build": 4 + int(4 * build_rise), "drop": 8, "break": 2}[section]
        for note in chord:
            loop.add(bar_start, loop.pad(note - 12, seconds_per_bar + 0.3, brightness), pan=(note % 5 - 2) * 0.2)
        if section == "build" and bar == 7:
            loop.add(bar_start, loop.riser(seconds_per_bar))

        for step in range(SIXTEENTHS_PER_BAR):
            t = bar_start + step * SECONDS_PER_SIXTEENTH
            on_beat = step % 4 == 0
            if section in ("drop",) or (section == "build" and bar >= 6):
                if on_beat:
                    loop.add(t, kick)
            if section == "drop" and step in (4, 12):
                loop.add(t, clap, pan=0.1)
            if section == "build" and bar == 7 and step >= 8:
                loop.add(t, clap * (0.3 + 0.08 * (step - 8)), pan=-0.1)
            if section == "intro" and step % 4 == 2:
                loop.add(t, loop.hat(0.05), pan=0.4)
            if section == "build" and step % 2 == 0:
                loop.add(t, loop.hat(0.06 + 0.06 * build_rise), pan=0.4)
            if section == "drop":
                loop.add(t, loop.hat(0.09 if step % 2 == 0 else 0.05), pan=0.35)
                if step in (0, 3, 6, 8, 10, 14):
                    loop.add(t, loop.bass(root + (12 if step == 10 else 0), 0.2))
                if step % 4 == 2:
                    loop.add(t, loop.lead(chord[(step // 4 + bar) % 3] + 24), pan=-0.3)
            if section in ("intro", "build", "drop") and step % 2 == 0:
                arp_note = chord[(step // 2) % 3] + 12 + (12 if step % 8 == 6 else 0)
                loop.add(t, loop.pluck(arp_note, 0.05 if section == "intro" else 0.07), pan=0.25 * math.sin(step))
            if section == "break":
                if step == 0:
                    loop.add(t, loop.sub(root, seconds_per_bar * 0.9))
                if bar == 13 and step % 2 == 1:
                    loop.add(t, loop.hat(0.07), pan=-0.4)
                if step % 8 == 0:
                    loop.add(t, loop.pluck(chord[0] + 24, 0.05))

    mixed = numpy.tanh(loop.buffer * 1.2)
    mixed *= 0.9 / max(1e-6, float(numpy.max(numpy.abs(mixed))))
    return numpy.ascontiguousarray(mixed.astype(numpy.float32))


@dataclass
class SynthSongConfig:
    """How loud the loop plays."""

    volume: float = 0.13


@node.meta(
    category="Paint Pulse",
    description="An original 32-second dance loop with an intro, build, drop and break, played on repeat.",
)
@node.process(execution="continuous", interval_ms=5)
class SynthSong:
    """The loop, streamed in real time and wrapped at its end."""

    audio = Output(Channels, rate=SAMPLE_RATE, dtype="f32", description="The song, in stereo")

    def __init__(self, config: SynthSongConfig) -> None:
        self.loop = render_the_loop()
        self.volume = 0.8
        self.configure(config)
        self.run_anchor_timestamp_ns: int | None = None
        self.start_sample_of_this_run = 0
        self.samples_emitted_in_this_run = 0
        self.position_in_the_loop = 0
        log.info("rendered the loop", seconds=self.loop.shape[1] / SAMPLE_RATE)

    def configure(self, config: SynthSongConfig) -> None:
        self.volume = min(1.0, max(0.0, float(config.volume)))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = ctx.time
        if self.run_anchor_timestamp_ns is None:
            self.run_anchor_timestamp_ns = now_ns
        samples_owed = ((now_ns - self.run_anchor_timestamp_ns) * SAMPLE_RATE) // NANOSECONDS_PER_SECOND
        if samples_owed - self.samples_emitted_in_this_run > MAXIMUM_BLOCKS_OF_CATCH_UP * BLOCK_SIZE:
            self.start_sample_of_this_run += samples_owed
            self.run_anchor_timestamp_ns = now_ns
            self.samples_emitted_in_this_run = 0
            samples_owed = 0
        while self.samples_emitted_in_this_run + BLOCK_SIZE <= samples_owed:
            self._publish_one_block(ctx)

    def _next_block(self) -> numpy.ndarray:
        length = self.loop.shape[1]
        indices = (self.position_in_the_loop + numpy.arange(BLOCK_SIZE)) % length
        self.position_in_the_loop = (self.position_in_the_loop + BLOCK_SIZE) % length
        return numpy.ascontiguousarray(self.loop[:, indices] * numpy.float32(self.volume))

    def _publish_one_block(self, ctx: RuntimeContextLimitedAccess) -> None:
        assert self.run_anchor_timestamp_ns is not None
        timestamp_ns = (
            self.run_anchor_timestamp_ns
            + (self.samples_emitted_in_this_run * NANOSECONDS_PER_SECOND) // SAMPLE_RATE
        )
        ctx.outputs.write(
            self.audio,
            self._next_block(),
            start_sample=self.start_sample_of_this_run + self.samples_emitted_in_this_run,
            timestamp_ns=timestamp_ns,
        )
        self.samples_emitted_in_this_run += BLOCK_SIZE
