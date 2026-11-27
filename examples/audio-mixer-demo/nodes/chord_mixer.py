# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Sums three voices into one chord.

Importable as `nodes.chord_mixer:ChordMixer`. Every Channels input declares the
same window contract, so the runtime resamples, mixes each voice down to mono
and frames it before a frame of this node runs: what this node receives is three
exactly-512-sample mono f32 windows, and mixing them is addition. Its own job is
deciding which three windows belong together, joined on each window's
`timestamp_ns` rather than on arrival order.
"""

import collections
from dataclasses import dataclass
from typing import Annotated, Callable, Final

import numpy
from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    computed,
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
class ChordVoiceWindow:
    """One voice's window, copied out of the block it arrived in so it can wait for its partners."""

    samples: numpy.ndarray
    first_sample_timestamp_ns: int

    @classmethod
    def copied_from(cls, block: Audio) -> "ChordVoiceWindow":
        """The block's mono samples, copied: a read's borrow ends with the frame it was made in."""
        return cls(
            samples=numpy.array(numpy.from_dlpack(block)[0], dtype=numpy.float32),
            first_sample_timestamp_ns=block.timestamp_ns,
        )


class WindowAccounting:
    """How many windows the mixer dropped and discarded, reported when it is closed."""

    def __init__(self) -> None:
        self.dropped_windows = 0
        self.realigned_windows = 0

    def close(self) -> None:
        if self.dropped_windows:
            log.warn(
                "voices ran far enough apart that windows were dropped waiting to be "
                "mixed",
                dropped_windows=self.dropped_windows,
            )
        if self.realigned_windows:
            log.info(
                "windows discarded to bring the voices onto one instant",
                realigned_windows=self.realigned_windows,
            )


@node.meta(description="Emits one mixed window per set of three time-aligned voice windows")
@node(execution="reactive")
def ChordMixer(
    *,
    voice_gain: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0),
            Description("The gain the summed voices are scaled by before clipping."),
        ]
    ] = Input(1.0),
    root_voice_from_upstream: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            CHORD_MIX_WINDOW,
            Description("The chord's root voice, converted to the mix window"),
        ]
    ],
    third_voice_from_upstream: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            CHORD_MIX_WINDOW,
            Description("The chord's third, converted to the mix window"),
        ]
    ],
    fifth_voice_from_upstream: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            CHORD_MIX_WINDOW,
            Description("The chord's fifth, converted to the mix window"),
        ]
    ],
    mixed_chord_to_downstream: OutputPin[Annotated[Audio, Description("The three voices summed")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Three voices in, at any rate and channel count; their mono sum out."""

    mixed_samples_published = 0
    pending_windows_by_port: dict[str, collections.deque[ChordVoiceWindow]] = {
        port_name: collections.deque(maxlen=MAXIMUM_PENDING_WINDOWS_PER_VOICE)
        for port_name in VOICE_INPUT_PORTS
    }
    # Built as a computed value so the node's stop closes it, which reports what it counted.
    window_accounting = computed(WindowAccounting)

    def hold(port_name: str, window: ChordVoiceWindow) -> None:
        pending = pending_windows_by_port[port_name]
        if len(pending) == MAXIMUM_PENDING_WINDOWS_PER_VOICE:
            window_accounting.get().dropped_windows += 1
            if window_accounting.get().dropped_windows == 1:
                log.warn(
                    "a voice ran further ahead of its partners than the mixer holds; "
                    "its oldest window is dropped",
                    port=port_name,
                )
        pending.append(window)

    def discard_heads_lagging_the_newest() -> bool:
        """Drop any head a whole window or more behind the newest one."""
        heads = {
            port_name: pending_windows_by_port[port_name][0]
            for port_name in VOICE_INPUT_PORTS
        }
        newest_ns = max(head.first_sample_timestamp_ns for head in heads.values())

        discarded_any = False
        for port_name, head in heads.items():
            if newest_ns - head.first_sample_timestamp_ns < ALIGNMENT_TOLERANCE_NS:
                continue
            pending_windows_by_port[port_name].popleft()
            window_accounting.get().realigned_windows += 1
            discarded_any = True
        return discarded_any

    def publish_one_mixed_window() -> None:
        nonlocal mixed_samples_published
        windows = [
            pending_windows_by_port[port_name].popleft()
            for port_name in VOICE_INPUT_PORTS
        ]

        mixed = windows[0].samples
        for window in windows[1:]:
            mixed += window.samples
        mixed *= voice_gain.get()

        # The voices are configured to sum below full scale; clipping bounds
        # what a raised gain can send to a speaker.
        numpy.clip(mixed, -1.0, 1.0, out=mixed)

        # The latest of the three: the instant by which every contribution has
        # started.
        first_sample_timestamp_ns = max(
            window.first_sample_timestamp_ns for window in windows
        )
        mixed_chord_to_downstream.write(
            mixed[numpy.newaxis, :],
            timestamp_ns=first_sample_timestamp_ns,
            start_sample=mixed_samples_published,
            sample_rate=CHORD_MIX_SAMPLE_RATE,
        )
        mixed_samples_published += CHORD_MIX_WINDOW_SIZE

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        # A reactive node's frame runs as soon as any one port has a full window,
        # so a call normally finds one voice ready and the others not yet.
        voice_inputs_by_port_name = {
            ROOT_VOICE_INPUT_PORT: root_voice_from_upstream,
            THIRD_VOICE_INPUT_PORT: third_voice_from_upstream,
            FIFTH_VOICE_INPUT_PORT: fifth_voice_from_upstream,
        }
        for port_name, voice_input in voice_inputs_by_port_name.items():
            block = voice_input.read()
            if block is None:
                continue
            hold(port_name, ChordVoiceWindow.copied_from(block))

        while all(pending_windows_by_port[port] for port in VOICE_INPUT_PORTS):
            # Dropping a lagging head can empty its voice, so the loop re-checks
            # before mixing.
            if discard_heads_lagging_the_newest():
                continue
            publish_one_mixed_window()

    return each_frame
