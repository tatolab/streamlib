# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Writes what a microphone captured to a WAV, for the signal analysis to read.

The tap cannot serve this: `streamlib tap` collects inside a bounded 500 ms
window (`TAP_SAMPLE_WINDOW`), which is a fifth of the known signal. That bound
is right for an observation verb and wrong for a measurement, so the
measurement is taken inside the graph instead — this is a consumer like any
other, reading the microphone's port over a real link.

Blocks are placed by their own timestamps rather than concatenated. Audio that
went missing then stays missing, which is what lets `known_audio_signal analyse`
see a hole; concatenating would close the gap and report a shorter signal that
still decodes.
"""

import os

import numpy

import known_audio_signal
from streamlib import AudioBlock, RuntimeContextLimitedAccess, input, log, processor

RESULT_MARKER = "MARKER:WAVEFORM_WRITTEN "

# The signal occupies 2.78 s and the source publishes a second of silence after
# it, so this covers the whole thing with runway for a capture that started
# late.
#
# Settable because the runway depends on what is upstream, not on the signal: a
# microphone captures forever and overshooting costs nothing, while a graph fed
# straight from the finite source runs out at 3.78 s and a window past that
# writes no waveform at all.
SECONDS_TO_RECORD = float(os.environ.get("STREAMLIB_CAPTURED_WAVEFORM_SECONDS", "5.0"))


@processor
class CapturedAudioWaveformRecorder:
    """Accumulates captured blocks, then writes them once as one waveform."""

    def __init__(self) -> None:
        self._blocks: "list[tuple[int, numpy.ndarray]]" = []
        self._samples_recorded = 0
        self._sample_rate = None
        self._written = False

    # The plan's profile for audio: order carries meaning, so blocks arrive in
    # the order they were published rather than skipping to the freshest. It
    # promises nothing about how many arrive.
    @input(delivery_profile="ordered")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None:
            return
        if self._written:
            # Still drained: a recorder that stopped reading would back its
            # link up behind the microphone, and a full link costs dropped
            # blocks rather than a stalled producer —
            # `PortMailbox::push_frame_from_inbound_link` evicts its oldest entry
            # whatever a port's profile says.
            return

        self._sample_rate = block.sample_rate
        # Mixed down here rather than in the analysis, so what is written is one
        # waveform whatever the device's channel count turned out to be.
        mono = block.samples.astype("<f8").mean(axis=1)
        self._blocks.append((block.first_sample_timestamp_ns, mono))
        self._samples_recorded += len(mono)

        if self._samples_recorded < SECONDS_TO_RECORD * block.sample_rate:
            return
        self._write_the_waveform()

    def _write_the_waveform(self) -> None:
        self._written = True
        path = os.environ["STREAMLIB_CAPTURED_WAVEFORM"]
        origin_ns = self._blocks[0][0]
        placed_at = [
            (timestamp_ns - origin_ns) * self._sample_rate // 1_000_000_000
            for timestamp_ns, _ in self._blocks
        ]
        waveform = numpy.zeros(
            placed_at[-1] + len(self._blocks[-1][1]), dtype="<f8"
        )
        for at, (_, mono) in zip(placed_at, self._blocks):
            waveform[at : at + len(mono)] = mono
        known_audio_signal.write_wav(path, waveform, self._sample_rate)
        log.info(f"{RESULT_MARKER}{path}")
