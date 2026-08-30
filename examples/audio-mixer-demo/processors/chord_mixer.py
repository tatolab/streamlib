# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Sums three voices into one chord.

The three inbound streams arrive at different rates, channel counts and block
sizes. Every input port here declares the same window contract, so the engine
resamples, mixes each voice down to mono and frames it before `process()` runs:
what this class receives is three exactly-512-sample mono f32 windows, and
mixing them is addition. There is no resampler, no ring buffer and no format
negotiation in this file, because none of that is a user processor's job.

What is this class's job is deciding *which* three windows belong together.
Each voice starts when its own helper interpreter does, so the three streams
sit on grids offset by tens of milliseconds; the windows are joined on
`first_sample_timestamp_ns` — the block-level A/V-sync primitive — never on
arrival order, which would freeze that startup skew in for the whole run.
"""

import collections

import numpy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    AudioBlock,
    AudioWindowContract,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    log,
    output,
    processor,
)

# One contract, declared on all three ports: the shape every voice is converted
# to, and the shape the mixed block is published in.
CHORD_MIX_WINDOW = AudioWindowContract(
    sample_rate=48_000,
    channels=1,
    dtype="f32",
    window_size=512,
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

# A voice whose partners have stopped may not queue without bound. Eight windows
# is ~85 ms per voice — far more slack than three sources paced by the same
# clock ever need, and small enough that a stalled voice costs bounded memory.
MAXIMUM_PENDING_WINDOWS_PER_VOICE = 8

WINDOW_DURATION_NS = (
    CHORD_MIX_WINDOW.window_size * 1_000_000_000 // CHORD_MIX_WINDOW.sample_rate
)
# How far apart three windows' first samples may sit and still be mixed as one
# instant. The voices start when their helper interpreters do, so their stamps
# sit on three grids offset by tens of milliseconds; pairing by arrival order
# would freeze that skew in permanently and publish a stamp two of the three
# contributions never came from. Half a window is the tightest the grids allow —
# each voice advances by exactly one window, so no amount of discarding aligns
# them closer than the residual between their anchors.
ALIGNMENT_TOLERANCE_NS = WINDOW_DURATION_NS // 2


@processor(description="Sums three voices into one chord")
class ChordMixer:
    """Emits one mixed window per set of three, one per voice."""

    def __init__(self, voice_gain: float = 1.0) -> None:
        self.voice_gain = float(voice_gain)
        self.pending_windows_by_port: dict[str, collections.deque] = {
            port_name: collections.deque(maxlen=MAXIMUM_PENDING_WINDOWS_PER_VOICE)
            for port_name in VOICE_INPUT_PORTS
        }
        self.dropped_window_count = 0
        self.realigned_window_count = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # A reactive process() runs as soon as any one port has a full window,
        # so a call normally finds one voice ready and the others not yet.
        for port_name in VOICE_INPUT_PORTS:
            window = ctx.inputs.read(port_name, into=AudioBlock)
            if window is None:
                continue
            self._hold(port_name, window)

        while all(self.pending_windows_by_port[port] for port in VOICE_INPUT_PORTS):
            # Re-checked after every discard: dropping a lagging head can empty
            # its voice, and the next window has to arrive before mixing again.
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
        """Drop any head more than half a window behind the newest one.

        This is the join: three streams are mixed because their samples cover
        the same instant, which is what `first_sample_timestamp_ns` is for —
        not because they happened to arrive in the same order.
        """
        heads = {
            port_name: self.pending_windows_by_port[port_name][0]
            for port_name in VOICE_INPUT_PORTS
        }
        newest_ns = max(head.first_sample_timestamp_ns for head in heads.values())

        discarded_any = False
        for port_name, head in heads.items():
            if newest_ns - head.first_sample_timestamp_ns <= ALIGNMENT_TOLERANCE_NS:
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

        # `samples` is a read-only view over the bag's own bytes, so the first
        # voice is copied and the rest are summed into that copy.
        mixed = windows[0].samples.astype("<f4") * self.voice_gain
        for window in windows[1:]:
            mixed += window.samples * self.voice_gain

        # The voices are configured to sum below full scale; clipping bounds
        # what a raised gain can send to a speaker.
        numpy.clip(mixed, -1.0, 1.0, out=mixed)

        # The three agree to within half a window by the join above, and the
        # mixed block names the latest of them — the instant by which every
        # contribution has started.
        first_sample_timestamp_ns = max(
            window.first_sample_timestamp_ns for window in windows
        )
        ctx.outputs.write(
            MIXED_CHORD_OUTPUT_PORT,
            {
                "samples": mixed.tobytes(),
                "sample_rate": CHORD_MIX_WINDOW.sample_rate,
                "channels": CHORD_MIX_WINDOW.channels,
                "sample_count": CHORD_MIX_WINDOW.window_size,
                "dtype": CHORD_MIX_WINDOW.dtype,
                "first_sample_timestamp_ns": first_sample_timestamp_ns,
            },
            first_sample_timestamp_ns,
        )

    @input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's root voice, converted to the mix window",
    )
    def root_voice_from_upstream(self) -> None: ...

    @input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's third, converted to the mix window",
    )
    def third_voice_from_upstream(self) -> None: ...

    @input(
        delivery_profile="ordered",
        audio_window=CHORD_MIX_WINDOW,
        description="The chord's fifth, converted to the mix window",
    )
    def fifth_voice_from_upstream(self) -> None: ...

    @output(description="The three voices summed, as AudioBlock bags")
    def mixed_chord_to_downstream(self) -> None: ...
