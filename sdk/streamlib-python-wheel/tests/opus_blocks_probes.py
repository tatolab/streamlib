# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The source and the two probes an Opus round trip is read through.

Each probe reports what it saw as JSON marker lines riding the child→parent
log forwarding — one line per packet or block rather than one array at the
end, so a run that stalls half way still says how far it got.

`EncodedAudioPacketProbe` is the one under test on the encoded side: it reads
the encoder's own output with `into=EncodedAudioPacket`, which is the whole
claim the cast makes. `DecodedAudioBlockProbe` reads the far side of the
decoder with `read_with_timestamp`, because the frame-header stamp is what the
trim and the derived-stamp assertions are about and no `into=` read yields it.

`StereoToneSource` states its own format rather than discovering one, so the
count the encoder follows is the test's fact and not the machine's. It carries
no microphone for the same reason the window-stage fixtures do not.
"""

import json
import math
import struct

from streamlib import (
    AudioBlock,
    EncodedAudioPacket,
    RuntimeContextLimitedAccess,
    input,
    log,
    monotonic_now_ns,
    output,
    processor,
)

ENCODED_PACKET_MARKER = "MARKER:ENCODED_PACKET "
ENCODED_PACKETS_COMPLETE_MARKER = "MARKER:ENCODED_PACKETS_COMPLETE"
DECODED_BLOCK_MARKER = "MARKER:DECODED_BLOCK "
DECODED_BLOCKS_COMPLETE_MARKER = "MARKER:DECODED_BLOCKS_COMPLETE"

# Opus's own clock, which is also what the encoder's window contract asks the
# stage to resample to — stated equal here so no resampler sits between the
# source and the measurement.
SOURCE_SAMPLE_RATE = 48_000
SOURCE_CHANNELS = 2

# 10 ms at 48 kHz: two of these fill one 20 ms Opus window, so the framing the
# encoder's contract does is visible rather than incidental.
SOURCE_FRAMES_PER_BLOCK = SOURCE_SAMPLE_RATE // 100

TONE_HZ = 440.0

# How far ahead of real time the publishing runs. Enough that a late
# `process()` costs the encoder nothing, and well inside the depth the
# consumer's windowed port is sized to, so no block is evicted between the two.
PUBLISHING_LEAD_NS = 100_000_000

# Enough packets that the ordering assertions are about a stream rather than a
# pair, and few enough that the run is short: 40 packets is 800 ms of audio.
ENCODED_PACKETS_REPORTED = 40

# Fewer than the encoded probe reports, so the decoded probe's entry packet is
# one the encoded probe also wrote down — the two are separate helper
# processes attaching at their own pace, and the cross-check needs the overlap.
DECODED_BLOCKS_REPORTED = 24

ENCODED_PORT = "encoded_audio_from_upstream"
DECODED_PORT = "audio_from_upstream"


@processor(execution="continuous", interval_ms=1)
class StereoToneSource:
    """Publishes a stereo tone at a stated rate, for the encoder to frame.

    Paced to stay a bounded lead ahead of the monotonic clock: a burst larger
    than the consumer's mailbox is evicted there, and the lost blocks would
    read as the codec's failure rather than this fixture's.
    """

    @output()
    def audio(self) -> None: ...

    def __init__(self) -> None:
        self._frames_published = 0
        self._first_sample_timestamp_ns = None

    def _is_far_enough_ahead(self, anchor_ns: int) -> bool:
        published_ns = self._frames_published * 1_000_000_000 // SOURCE_SAMPLE_RATE
        elapsed_ns = monotonic_now_ns() - anchor_ns
        return published_ns - elapsed_ns > PUBLISHING_LEAD_NS

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        anchor_ns = self._first_sample_timestamp_ns
        if anchor_ns is None:
            anchor_ns = monotonic_now_ns()
            self._first_sample_timestamp_ns = anchor_ns
        elif self._is_far_enough_ahead(anchor_ns):
            return

        at = self._frames_published
        scalars = []
        for offset in range(SOURCE_FRAMES_PER_BLOCK):
            instant = (at + offset) / SOURCE_SAMPLE_RATE
            scalars.extend([math.sin(math.tau * TONE_HZ * instant)] * SOURCE_CHANNELS)

        ctx.outputs.write(
            "audio",
            {
                "samples": struct.pack(f"<{len(scalars)}f", *scalars),
                "sample_rate": SOURCE_SAMPLE_RATE,
                "channels": SOURCE_CHANNELS,
                "sample_count": SOURCE_FRAMES_PER_BLOCK,
                "dtype": "f32",
                # Derived from the samples before it rather than read fresh, so
                # the stamps describe one gapless stream even though the
                # publishing runs ahead of real time.
                "first_sample_timestamp_ns": (
                    anchor_ns + at * 1_000_000_000 // SOURCE_SAMPLE_RATE
                ),
            },
        )
        self._frames_published += SOURCE_FRAMES_PER_BLOCK


@processor
class EncodedAudioPacketProbe:
    """Casts the encoder's output and reports each packet's wire fields.

    No sync-point gate, unlike the video probe: every Opus packet is one, so
    a reader enters wherever it attached.
    """

    def __init__(self) -> None:
        self.packets_admitted = 0

    @input(delivery_profile="ordered")
    def encoded_audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.packets_admitted >= ENCODED_PACKETS_REPORTED:
            return
        # The frame-header stamp beside the bag, because what the decoder
        # anchors its run on is the packet's stamp and the cross-check needs
        # both halves off one read.
        bag, timestamp_ns = ctx.inputs.read_with_timestamp(ENCODED_PORT)
        if bag is None:
            return
        packet = EncodedAudioPacket.from_bag(bag)
        self.packets_admitted += 1
        log.info(
            ENCODED_PACKET_MARKER
            + json.dumps(
                {
                    "codec": packet.codec,
                    "is_sync_point": packet.is_sync_point,
                    "group_index": packet.group_index,
                    "sequence_index": packet.sequence_index,
                    "sample_rate": packet.sample_rate,
                    "channels": packet.channels,
                    "sample_count": packet.sample_count,
                    "pre_skip": packet.pre_skip,
                    "byte_count": len(packet.opus_packet_bytes),
                    "timestamp_ns": timestamp_ns,
                }
            )
        )
        if self.packets_admitted == ENCODED_PACKETS_REPORTED:
            log.info(ENCODED_PACKETS_COMPLETE_MARKER)


@processor
class DecodedAudioBlockProbe:
    """Reports each decoded block's format, its length and its stamp.

    The stamp comes off the frame header rather than the bag so it is the one
    the transport carried, which is what a re-stamp at publication would move.
    """

    def __init__(self) -> None:
        self.blocks_admitted = 0

    @input(delivery_profile="ordered")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.blocks_admitted >= DECODED_BLOCKS_REPORTED:
            return
        bag, timestamp_ns = ctx.inputs.read_with_timestamp(DECODED_PORT)
        if bag is None:
            return
        block = AudioBlock.from_bag(bag)
        self.blocks_admitted += 1
        log.info(
            DECODED_BLOCK_MARKER
            + json.dumps(
                {
                    "sample_rate": block.sample_rate,
                    "channels": block.channels,
                    "dtype": block.dtype,
                    "sample_count": block.sample_count,
                    # Counted off the samples themselves rather than trusted
                    # from `sample_count`, so a block declaring a length it
                    # does not carry is caught rather than believed.
                    "scalars_read": int(block.samples.size),
                    "timestamp_ns": timestamp_ns,
                }
            )
        )
        if self.blocks_admitted == DECODED_BLOCKS_REPORTED:
            log.info(DECODED_BLOCKS_COMPLETE_MARKER)
