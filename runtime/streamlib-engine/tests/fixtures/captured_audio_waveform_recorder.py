# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Writes what a microphone captured to a WAV, for the signal analysis to read.

The measurement is taken inside the graph, over the whole known signal — this
is a consumer like any other, reading the microphone's port over a real link.

Blocks are placed by their own start samples rather than concatenated. Audio that
went missing then stays missing, which is what lets `known_audio_signal analyse`
see a hole; concatenating would close the gap and report a shorter signal that
still decodes.
"""

import os
from typing import Annotated, Callable

import numpy

import known_audio_signal
from tatolab.stream import Audio, InputPin, RuntimeContextLimitedAccess, StreamDelivery, log, node

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


def waveform_placed_by_start_sample(
    blocks: "list[tuple[int, numpy.ndarray]]",
) -> numpy.ndarray:
    """One mono waveform holding each block at its start sample, the first at zero."""
    origin_sample = blocks[0][0]
    waveform_length = max(
        start_sample - origin_sample + len(mono) for start_sample, mono in blocks
    )
    waveform = numpy.zeros(waveform_length, dtype="<f8")
    for start_sample, mono in blocks:
        at = start_sample - origin_sample
        waveform[at : at + len(mono)] = mono
    return waveform


@node(execution="reactive")
def CapturedAudioWaveformRecorder(
    *,
    audio_from_upstream: InputPin[Annotated[Audio, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Accumulates captured blocks, then writes them once as one waveform."""

    _blocks: "list[tuple[int, numpy.ndarray]]" = []
    samples_recorded = 0
    sample_rate = None
    written = False

    def write_the_waveform() -> None:
        nonlocal written
        written = True
        path = os.environ["STREAMLIB_CAPTURED_WAVEFORM"]
        waveform = waveform_placed_by_start_sample(_blocks)
        known_audio_signal.write_wav(path, waveform, sample_rate)
        log.info(f"{RESULT_MARKER}{path}")

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal sample_rate, samples_recorded
        block = audio_from_upstream.read()
        if block is None:
            return
        if written:
            # Still drained: a recorder that stopped reading would back its
            # link up behind the microphone, and a full link costs dropped
            # blocks rather than a stalled producer —
            # `PortMailbox::push_frame_from_inbound_link` evicts its oldest entry
            # whatever a port's profile says.
            return

        sample_rate = block.sample_rate
        # Mixed down here rather than in the analysis, so what is written is one
        # waveform whatever the device's channel count turned out to be.
        mono = numpy.from_dlpack(block).astype("<f8").mean(axis=0)
        if _blocks and block.start_sample < _blocks[-1][0]:
            # A restarted source counts its samples from zero again, so the
            # blocks before it share no timeline with the ones after.
            _blocks.clear()
            samples_recorded = 0
        _blocks.append((block.start_sample, mono))
        samples_recorded += len(mono)

        if samples_recorded < SECONDS_TO_RECORD * block.sample_rate:
            return
        write_the_waveform()

    return each_frame
