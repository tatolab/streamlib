# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The source and the two probes an Opus round trip is read through.

Each probe reports what it saw as JSON marker lines on `tatolabd`'s standard
error — one line per packet or block rather than one array at the
end, so a run that stalls half way still says how far it got.

The encoded probe reads the encoder's output the way a moq-dev player does:
each message one bare Opus packet, stamped on the message, and the port's
stream facts carrying the `OpusHead`. The decoded probe reads
each Channels block's own stamp, because that stamp is what the trim and the
derived-stamp assertions are about.

`StereoToneSource` states its own format rather than discovering one, so the
count the encoder follows is the test's fact and not the machine's. It carries
no microphone for the same reason the window-stage fixtures do not.
"""

import json
import math
import struct
from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Audio,
    EncodedAudio,
    EncodedCodecs,
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    log,
    monotonic_now_ns,
    node,
)
from tatolab.stream.stream_facts import PacketsStreamFacts

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

@node(execution="continuous", interval_ms=1)
def StereoToneSource(
    *,
    audio: OutputPin[Audio],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Publishes a stereo tone at a stated rate, for the encoder to frame.

    Paced to stay a bounded lead ahead of the monotonic clock: a burst larger
    than the consumer's mailbox is evicted there, and the lost blocks would
    read as the codec's failure rather than this fixture's.
    """

    frames_published = 0
    first_sample_timestamp_ns = None

    def is_far_enough_ahead(anchor_ns: int) -> bool:
        published_ns = frames_published * 1_000_000_000 // SOURCE_SAMPLE_RATE
        elapsed_ns = monotonic_now_ns() - anchor_ns
        return published_ns - elapsed_ns > PUBLISHING_LEAD_NS

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal first_sample_timestamp_ns, frames_published
        anchor_ns = first_sample_timestamp_ns
        if anchor_ns is None:
            anchor_ns = monotonic_now_ns()
            first_sample_timestamp_ns = anchor_ns
        elif is_far_enough_ahead(anchor_ns):
            return

        at = frames_published
        instants = (at + numpy.arange(SOURCE_FRAMES_PER_BLOCK)) / SOURCE_SAMPLE_RATE
        tone = numpy.sin(math.tau * TONE_HZ * instants).astype(numpy.float32)
        audio.write(
            numpy.ascontiguousarray(numpy.stack([tone] * SOURCE_CHANNELS)),
            # Derived from the samples before it rather than read fresh, so the
            # stamps describe one gapless stream even though the publishing
            # runs ahead of real time.
            timestamp_ns=anchor_ns + at * 1_000_000_000 // SOURCE_SAMPLE_RATE,
            start_sample=at,
            sample_rate=SOURCE_SAMPLE_RATE,
        )
        frames_published += SOURCE_FRAMES_PER_BLOCK

    return each_frame


def opus_head_channels_and_pre_skip(hex_encoded_opus_head: str) -> tuple[int, int]:
    """The channel count and pre-skip an `OpusHead` (RFC 7845 §5.1) carries."""
    opus_head = bytes.fromhex(hex_encoded_opus_head)
    if opus_head[:8] != b"OpusHead":
        raise ValueError(f"not an OpusHead: {hex_encoded_opus_head}")
    (pre_skip,) = struct.unpack_from("<H", opus_head, 10)
    return opus_head[9], pre_skip


@node(execution="reactive")
def EncodedOpusMediaFrameProbe(
    *,
    encoded_audio_from_upstream: InputPin[
        Annotated[EncodedAudio, StreamDelivery("ordered"), EncodedCodecs("opus")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the encoder's output as Opus packets and reports each one.

    No group gate, unlike the video probe: every Opus packet opens a group, so
    a reader enters wherever it attached.
    """

    packets_admitted = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal packets_admitted
        if packets_admitted >= ENCODED_PACKETS_REPORTED:
            return
        message = encoded_audio_from_upstream.read_message()
        if message is None:
            return
        opus_packet = bytes(memoryview(message.payload.payload).cast("B"))
        upstream_stream_facts = encoded_audio_from_upstream.upstream_stream_facts()
        if not isinstance(upstream_stream_facts, PacketsStreamFacts):
            raise RuntimeError(
                "the encoder's port carries no Packets stream facts beside a packet it published"
            )
        opus_head_channels, pre_skip = opus_head_channels_and_pre_skip(
            upstream_stream_facts.codec_configuration_hex
        )
        packets_admitted += 1
        log.info(
            ENCODED_PACKET_MARKER
            + json.dumps(
                {
                    "starts_group": message.starts_group,
                    "lost_since_last_read": message.lost_since_last_read,
                    "toc_byte": opus_packet[0] if opus_packet else None,
                    "timestamp_ns": message.timestamp_ns,
                    "byte_count": len(opus_packet),
                    "stream_facts_codec": upstream_stream_facts.rfc6381_codec,
                    "stream_facts_sample_rate": upstream_stream_facts.sample_rate,
                    "stream_facts_channel_count": upstream_stream_facts.channel_count,
                    "opus_head_channels": opus_head_channels,
                    "pre_skip": pre_skip,
                }
            )
        )
        if packets_admitted == ENCODED_PACKETS_REPORTED:
            log.info(ENCODED_PACKETS_COMPLETE_MARKER)

    return each_frame


@node(execution="reactive")
def DecodedAudioBlockProbe(
    *,
    audio_from_upstream: InputPin[Annotated[Audio, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports each decoded block's format, its length and its stamp.

    The stamp comes off the frame header rather than the payload so it is the one
    the transport carried, which is what a re-stamp at publication would move.
    """

    blocks_admitted = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal blocks_admitted
        if blocks_admitted >= DECODED_BLOCKS_REPORTED:
            return
        block = audio_from_upstream.read()
        if block is None:
            return
        blocks_admitted += 1
        log.info(
            DECODED_BLOCK_MARKER
            + json.dumps(
                {
                    "sample_rate": block.sample_rate,
                    "channels": block.channel_count,
                    "dtype": block.dtype,
                    "sample_count": block.samples_per_channel,
                    "start_sample": block.start_sample,
                    # Counted off the samples themselves rather than trusted
                    # from `samples_per_channel`, so a block declaring a length
                    # it does not carry is caught rather than believed.
                    "scalars_read": int(numpy.from_dlpack(block).size),
                    "timestamp_ns": block.timestamp_ns,
                }
            )
        )
        if blocks_admitted == DECODED_BLOCKS_REPORTED:
            log.info(DECODED_BLOCKS_COMPLETE_MARKER)

    return each_frame
