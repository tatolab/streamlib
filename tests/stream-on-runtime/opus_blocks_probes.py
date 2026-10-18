# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The source and the two probes an Opus round trip is read through.

Each probe reports what it saw as JSON marker lines on `tatolabd`'s standard
error — one line per packet or block rather than one array at the
end, so a run that stalls half way still says how far it got.

The encoded probe reads the encoder's output the way a moq-dev player does:
each message a media frame — the varint timestamp, then one Opus packet — and
the port's catalog entry carrying the `OpusHead`. The decoded probe reads with
`read_with_timestamp`, because the frame-header stamp is what the trim and
the derived-stamp assertions are about.

`StereoToneSource` states its own format rather than discovering one, so the
count the encoder follows is the test's fact and not the machine's. It carries
no microphone for the same reason the window-stage fixtures do not.
"""

import json
import math
import struct

from hang_media_frame_reading import split_hang_legacy_container_media_frame
from tatolab.stream import (
    AudioBlock,
    RuntimeContextLimitedAccess,
    log,
    monotonic_now_ns,
    node,
)
from tatolab.stream.msgpack import pack

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


@node(execution="continuous", interval_ms=1)
class StereoToneSource:
    """Publishes a stereo tone at a stated rate, for the encoder to frame.

    Paced to stay a bounded lead ahead of the monotonic clock: a burst larger
    than the consumer's mailbox is evicted there, and the lost blocks would
    read as the codec's failure rather than this fixture's.
    """

    @node.output()
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
            pack(
                {
                    "samples": struct.pack(f"<{len(scalars)}f", *scalars),
                    "sample_rate": SOURCE_SAMPLE_RATE,
                    "channels": SOURCE_CHANNELS,
                    "sample_count": SOURCE_FRAMES_PER_BLOCK,
                    "dtype": "f32",
                    # Derived from the samples before it rather than read fresh,
                    # so the stamps describe one gapless stream even though the
                    # publishing runs ahead of real time.
                    "first_sample_timestamp_ns": (
                        anchor_ns + at * 1_000_000_000 // SOURCE_SAMPLE_RATE
                    ),
                }
            ),
        )
        self._frames_published += SOURCE_FRAMES_PER_BLOCK


def opus_head_channels_and_pre_skip(hex_encoded_opus_head: str) -> tuple[int, int]:
    """The channel count and pre-skip an `OpusHead` (RFC 7845 §5.1) carries."""
    opus_head = bytes.fromhex(hex_encoded_opus_head)
    if opus_head[:8] != b"OpusHead":
        raise ValueError(f"not an OpusHead: {hex_encoded_opus_head}")
    (pre_skip,) = struct.unpack_from("<H", opus_head, 10)
    return opus_head[9], pre_skip


@node
class EncodedOpusMediaFrameProbe:
    """Reads the encoder's output as media frames and reports each packet.

    No group gate, unlike the video probe: every Opus packet opens a group, so
    a reader enters wherever it attached.
    """

    def __init__(self) -> None:
        self.packets_admitted = 0

    @node.input(delivery_profile="ordered")
    def encoded_audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.packets_admitted >= ENCODED_PACKETS_REPORTED:
            return
        message = ctx.inputs.read_message(ENCODED_PORT)
        if message is None:
            return
        varint_timestamp_microseconds, opus_packet = split_hang_legacy_container_media_frame(
            message.payload
        )
        catalog_entry = ctx.inputs.inbound_link_catalog_entry(ENCODED_PORT)
        opus_head_channels, pre_skip = opus_head_channels_and_pre_skip(
            catalog_entry["entry"]["description"]
        )
        self.packets_admitted += 1
        log.info(
            ENCODED_PACKET_MARKER
            + json.dumps(
                {
                    "starts_group": message.starts_group,
                    "lost_since_last_read": message.lost_since_last_read,
                    "varint_timestamp_microseconds": varint_timestamp_microseconds,
                    "timestamp_ns": message.timestamp_ns,
                    "byte_count": len(opus_packet),
                    "catalog_section": catalog_entry["section"],
                    "catalog_codec": catalog_entry["entry"]["codec"],
                    "catalog_sample_rate": catalog_entry["entry"]["sampleRate"],
                    "catalog_number_of_channels": catalog_entry["entry"]["numberOfChannels"],
                    "opus_head_channels": opus_head_channels,
                    "pre_skip": pre_skip,
                }
            )
        )
        if self.packets_admitted == ENCODED_PACKETS_REPORTED:
            log.info(ENCODED_PACKETS_COMPLETE_MARKER)


@node
class DecodedAudioBlockProbe:
    """Reports each decoded block's format, its length and its stamp.

    The stamp comes off the frame header rather than the payload so it is the one
    the transport carried, which is what a re-stamp at publication would move.
    """

    def __init__(self) -> None:
        self.blocks_admitted = 0

    @node.input(delivery_profile="ordered")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.blocks_admitted >= DECODED_BLOCKS_REPORTED:
            return
        payload, timestamp_ns = ctx.inputs.read_with_timestamp(DECODED_PORT)
        if payload is None:
            return
        block = AudioBlock.from_bytes(payload)
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
