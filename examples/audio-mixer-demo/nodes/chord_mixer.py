# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Sums three voices into one chord.

Importable as `nodes.chord_mixer:ChordMixer`. Every input port declares the
same window contract, so the engine resamples, mixes each voice down to mono
and frames it before `process()` runs: what this node receives is three
exactly-512-sample mono f32 windows, and mixing them is addition. Its own job is
deciding which three windows belong together, joined on
`first_sample_timestamp_ns` rather than on arrival order.
"""

import collections
from dataclasses import dataclass
from typing import Final

import numpy
from tatolab.stream import (
    AudioBlock,
    AudioWindowContract,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    catalog,
    log,
    node,
)

CHORD_MIX_SAMPLE_RATE: Final = 48_000
CHORD_MIX_CHANNELS: Final = 1
CHORD_MIX_DTYPE: Final = "f32"
CHORD_MIX_WINDOW_SIZE: Final = 512

# The shape every voice is converted to, and the shape the mixed block is
# published in.
CHORD_MIX_WINDOW = AudioWindowContract(
    sample_rate=CHORD_MIX_SAMPLE_RATE,
    channels=CHORD_MIX_CHANNELS,
    dtype=CHORD_MIX_DTYPE,
    window_size=CHORD_MIX_WINDOW_SIZE,
)

ROOT_VOICE_INPUT_PORT = "root_voice_from_upstream"
THIRD_VOICE_INPUT_PORT = "third_voice_from_upstream"
FIFTH_VOICE_INPUT_PORT = "fifth_voice_from_upstream"
VOICE_INPUT_PORTS = (
    ROOT_VOICE_INPUT_PORT,
    THIRD_VOICE_INPUT_PORT,
    FIFTH_VOICE_INPUT_PORT,
)

MIXED_CHORD_OUTPUT_PORT = "mixed_chord_to_downstream"

# ~85 ms per voice: far more slack than three sources paced by one clock need,
# and a bound on what a stalled voice's partners can queue.
MAXIMUM_PENDING_WINDOWS_PER_VOICE = 8

WINDOW_DURATION_NS = CHORD_MIX_WINDOW_SIZE * 1_000_000_000 // CHORD_MIX_SAMPLE_RATE
# A head is discarded only once it is a whole window or more behind the newest.
# Each voice anchors when its own interpreter first ticks, so the three stamp
# grids carry sub-window offsets that discarding cannot remove; a tighter bound
# never converges and thrashes a window per mix forever, while one window
# strictly reduces the spread and cannot overshoot.
ALIGNMENT_TOLERANCE_NS = WINDOW_DURATION_NS


@dataclass
class ChordMixerConfig:
    """ChordMixer's settings, as `stream_builder.add(ChordMixer, config={...})` spells them."""

    voice_gain: float = 1.0


@node
class ChordMixer:
    """Emits one mixed window per set of three time-aligned voice windows."""

    @node.input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's root voice, converted to the mix window",
    )
    def root_voice_from_upstream(self) -> AudioBlock: ...

    @node.input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's third, converted to the mix window",
    )
    def third_voice_from_upstream(self) -> AudioBlock: ...

    @node.input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's fifth, converted to the mix window",
    )
    def fifth_voice_from_upstream(self) -> AudioBlock: ...

    @node.output(
        description="The three voices summed, as AudioBlock messages",
        catalog=catalog.BinaryEntry(mode="snapshot", mime="application/msgpack"),
    )
    def mixed_chord_to_downstream(self) -> AudioBlock: ...

    def __init__(self, config: ChordMixerConfig) -> None:
        self.voice_gain = float(config.voice_gain)
        self.pending_windows_by_port: dict[str, collections.deque[AudioBlock]] = {
            port_name: collections.deque(maxlen=MAXIMUM_PENDING_WINDOWS_PER_VOICE)
            for port_name in VOICE_INPUT_PORTS
        }
        self.dropped_window_count = 0
        self.realigned_window_count = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # A reactive `process()` runs as soon as any one port has a full window,
        # so a call normally finds one voice ready and the others not yet.
        for port_name in VOICE_INPUT_PORTS:
            window = ctx.inputs.read(port_name, into=AudioBlock)
            if window is None:
                continue
            self._hold(port_name, window)

        while all(self.pending_windows_by_port[port] for port in VOICE_INPUT_PORTS):
            # Dropping a lagging head can empty its voice, so the loop re-checks
            # before mixing.
            if self._discard_heads_lagging_the_newest():
                continue
            self._publish_one_mixed_window(ctx)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        if self.dropped_window_count:
            log.warn(
                "voices ran far enough apart that windows were dropped waiting to be "
                "mixed",
                dropped_windows=self.dropped_window_count,
            )
        if self.realigned_window_count:
            log.info(
                "windows discarded to bring the voices onto one instant",
                realigned_windows=self.realigned_window_count,
            )

    def _hold(self, port_name: str, window: AudioBlock) -> None:
        pending = self.pending_windows_by_port[port_name]
        if len(pending) == MAXIMUM_PENDING_WINDOWS_PER_VOICE:
            self.dropped_window_count += 1
            if self.dropped_window_count == 1:
                log.warn(
                    "a voice ran further ahead of its partners than the mixer holds; "
                    "its oldest window is dropped",
                    port=port_name,
                )
        pending.append(window)

    def _discard_heads_lagging_the_newest(self) -> bool:
        """Drop any head a whole window or more behind the newest one."""
        heads = {
            port_name: self.pending_windows_by_port[port_name][0]
            for port_name in VOICE_INPUT_PORTS
        }
        newest_ns = max(head.first_sample_timestamp_ns for head in heads.values())

        discarded_any = False
        for port_name, head in heads.items():
            if newest_ns - head.first_sample_timestamp_ns < ALIGNMENT_TOLERANCE_NS:
                continue
            self.pending_windows_by_port[port_name].popleft()
            self.realigned_window_count += 1
            discarded_any = True
        return discarded_any

    def _publish_one_mixed_window(self, ctx: RuntimeContextLimitedAccess) -> None:
        windows = [
            self.pending_windows_by_port[port_name].popleft()
            for port_name in VOICE_INPUT_PORTS
        ]

        # `samples` is a read-only view over the message's own bytes, so the
        # first voice is copied and the rest are summed into that copy.
        mixed = windows[0].samples.astype("<f4")
        for window in windows[1:]:
            mixed += window.samples
        mixed *= self.voice_gain

        # The voices are configured to sum below full scale; clipping bounds
        # what a raised gain can send to a speaker.
        numpy.clip(mixed, -1.0, 1.0, out=mixed)

        # The latest of the three: the instant by which every contribution has
        # started.
        first_sample_timestamp_ns = max(
            window.first_sample_timestamp_ns for window in windows
        )
        mixed_chord_block = AudioBlock(
            samples=mixed.tobytes(),
            sample_rate=CHORD_MIX_SAMPLE_RATE,
            channels=CHORD_MIX_CHANNELS,
            sample_count=CHORD_MIX_WINDOW_SIZE,
            dtype=CHORD_MIX_DTYPE,
            first_sample_timestamp_ns=first_sample_timestamp_ns,
        )
        ctx.outputs.write(
            MIXED_CHORD_OUTPUT_PORT,
            mixed_chord_block.to_bytes(),
            timestamp_ns=first_sample_timestamp_ns,
        )
