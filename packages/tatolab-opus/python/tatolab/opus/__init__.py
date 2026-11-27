# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Opus encode and decode for StreamLib: `OpusEncoder` and `OpusDecoder`.

An extension wheel that links no engine crate; each node runs in its own
node interpreter like any other Python node, and is defined here so its
type is `tatolab.opus:OpusEncoder` and `tatolab.opus:OpusDecoder`.

Both carry bare Opus packets, one per message, each opening its own group and
stamped on the message itself. The codec state is
this wheel's Rust; what lives here is the port surface, the stream facts and
the log lines, because a helper process's Rust has no `tracing` subscriber.
"""

from __future__ import annotations

from typing import Annotated, Callable, Literal

import numpy

from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    EncodedAudio,
    EncodedCodecs,
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
from tatolab.stream.stream_facts import PacketsStreamFacts

from . import _native

__all__ = ["OpusDecoder", "OpusEncoder"]

ENCODED_AUDIO_PORT = "encoded_audio"

#: Opus's own clock, which the encoder's window contract resamples to.
OPUS_SAMPLE_RATE_HZ = 48_000
NANOSECONDS_PER_SECOND = 1_000_000_000
#: One 20 ms Opus frame at 48 kHz, in per-channel samples.
SAMPLES_IN_ONE_OPUS_WINDOW = 960
OPUS_RFC6381_CODEC_STRING = "opus"
#: libopus takes its bitrate as a C `opus_int32`.
_HIGHEST_BITRATE_BPS_LIBOPUS_TAKES = 2**31 - 1

OpusEncoderApplication = Literal["audio", "voip", "lowdelay"]


def _log_codec_notices(notices: "list[tuple[Literal['info', 'warn'], str]]") -> None:
    for level, message in notices:
        if level == "warn":
            log.warn(message)
        else:
            log.info(message)


@node.meta(
    description=(
        "Encodes 20 ms windows of audio to Opus via libopus.\n\n"
        "`audio` declares `AudioWindowContract(sample_rate=48000, dtype=\"f32\", "
        "window_size=960, hop=960)` and states no channel count, so the engine resamples "
        "to Opus's own clock, converts to `f32` and frames into 20 ms windows while the "
        "count follows whatever the source publishes. One to eight channels; more is "
        "refused by name. `encoded_audio` publishes one bare Opus packet per message, stamped "
        "with the instant of its first sample and each one opening its own group. The port's stream facts are `PacketsStreamFacts` — `opus`, 48 000 Hz, the "
        "channel count — carrying the `OpusHead` hex-encoded as `codec_configuration_hex`, so a "
        "decoder learns the channel count and the encoder's lookahead before the first "
        "packet.\n\n"
        "The libopus encoder is minted from the first window's channel count — one or two "
        "channels as a single stream, three to eight as a multistream under channel "
        "mapping family 1 — and re-mints when the source's count changes, replacing the "
        "port's stream facts with ones naming the new count. In-band FEC and DTX are off "
        "and are not knobs."
    ),
    conversion=True,
)
@node(scheduling="high")
def OpusEncoder(
    *,
    bitrate_bps: Input[
        Annotated[
            int | None,
            NumberRange(minimum=1, maximum=_HIGHEST_BITRATE_BPS_LIBOPUS_TAKES),
            Description(
                "Target bitrate in bits per second. Unset, libopus picks its own from the "
                "sample rate and channel count."
            ),
        ]
    ] = Input(None),
    application: Input[
        Annotated[
            OpusEncoderApplication,
            Description(
                "Which libopus tuning to encode for: `audio` for broadcast and high fidelity, "
                "`voip` for speech, `lowdelay` for the lowest latency at a cost in quality."
            ),
        ]
    ] = Input("audio"),
    audio: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            AudioWindowContract(
                sample_rate=OPUS_SAMPLE_RATE_HZ,
                dtype="f32",
                window_size=SAMPLES_IN_ONE_OPUS_WINDOW,
                hop=SAMPLES_IN_ONE_OPUS_WINDOW,
            ),
            Description(
                "Audio to encode, resampled and framed to 20 ms at Opus's own rate in the "
                "source's own channel count"
            ),
        ]
    ],
    encoded_audio: OutputPin[
        Annotated[
            EncodedAudio,
            EncodedCodecs(OPUS_RFC6381_CODEC_STRING),
            Description("Opus packets, one per message"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Audio windows in, Opus packets out.

    Both inputs are live: a new value builds an encoder of it off the frame thread, and the
    next window is encoded with it, its port's stream facts stated again before its first
    packet.
    """
    encoding = computed(lambda: _OpusEncoding(bitrate_bps.get(), application.get()))
    stream_facts_owed_to_the_port: "_native.OpusEncoderPortStreamFacts | None" = None
    encoder_owing_them: "_OpusEncoding | None" = None

    def encode_each_window(frame_ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal stream_facts_owed_to_the_port, encoder_owing_them
        del frame_ctx
        encoder = encoding.get()
        if encoder is not encoder_owing_them:
            # A new encoder states its own stream facts before its first packet.
            encoder_owing_them, stream_facts_owed_to_the_port = encoder, None
        while (window := audio.read()) is not None:
            interleaved_samples = numpy.ascontiguousarray(numpy.from_dlpack(window).T)
            try:
                opus_packet_bytes = encoder.native.encode_one_window(
                    interleaved_samples.tobytes(),
                    window.sample_rate,
                    window.channel_count,
                    window.samples_per_channel,
                    window.dtype,
                )
            finally:
                _log_codec_notices(encoder.native.take_notices())
            if opus_packet_bytes is None:
                continue
            owed = encoder.native.take_stream_facts_owed_to_the_port()
            if owed is not None:
                stream_facts_owed_to_the_port = owed
            # Set ahead of the first packet it describes, and kept owed until
            # the port takes it.
            stream_facts = stream_facts_owed_to_the_port
            if stream_facts is not None:
                encoded_audio.set_stream_facts(
                    PacketsStreamFacts(
                        rfc6381_codec=OPUS_RFC6381_CODEC_STRING,
                        sample_rate=stream_facts.sample_rate,
                        channel_count=stream_facts.channel_count,
                        codec_configuration_hex=stream_facts.codec_configuration_hex,
                    ),
                )
                stream_facts_owed_to_the_port = None
            # Stamped with the instant the packet's first sample was captured,
            # never the moment it was published.
            encoded_audio.write(
                opus_packet_bytes, timestamp_ns=window.timestamp_ns, starts_group=True
            )

    return encode_each_window


class _OpusEncoding:
    """One libopus encoder of a bitrate and a tuning, which says how many packets it encoded
    when it is closed."""

    def __init__(self, bitrate_bps: "int | None", application: str) -> None:
        self.native = _native.AudioWindowToEncodedPacketEncoder(bitrate_bps, application)

    def close(self) -> None:
        log.info("OpusEncoder: encoder closed", packets_encoded=self.native.packets_encoded)


@node.meta(
    description=(
        "Decodes Opus packets to audio blocks via libopus.\n\n"
        "`encoded_audio` takes bare Opus packets, one per message as `OpusEncoder` "
        "publishes them, and declares no window contract: an encoded link carries whole "
        "packets. The channel count and the encoder's "
        "lookahead come from the `OpusHead` in the upstream port's stream facts. Facts "
        "carrying no `OpusHead` name the channel count; with no facts at all, the "
        "first packet's TOC fixes one or two channels for the run. Either way nothing is "
        "trimmed. An empty message, upstream facts naming a codec other "
        "than `opus`, an `OpusHead` that does not read, and a `pre_skip` past the "
        "5 760 samples one Opus packet can span are refused by name. `audio` publishes "
        "Channels blocks — `f32` at 48 000 Hz in the stream's own channel count.\n\n"
        "The entry is read as it stands when each packet decodes. When the upstream "
        "re-mints for a new channel count, packets still queued on the link decode under "
        "the new entry until the first packet written in the new count arrives.\n\n"
        "It enters the stream at any packet, since every Opus packet opens a group, and "
        "trims the encoder's `pre_skip` lookahead at entry, so the first block after entry "
        "is short by exactly that many samples. Stamps derive from the entry packet's own "
        "stamp plus the samples emitted since, so the first emitted sample is the stamped "
        "instant.\n\n"
        "A read reporting lost packets resets the decoder, re-enters at that packet, and "
        "logs how many it did not see. Nothing is invented to bridge the loss — no "
        "concealment, no FEC decode."
    ),
    conversion=True,
)
@node(scheduling="high")
def OpusDecoder(
    *,
    encoded_audio: InputPin[
        Annotated[
            EncodedAudio,
            EncodedCodecs(OPUS_RFC6381_CODEC_STRING),
            StreamDelivery("ordered"),
            Description("Opus packets to decode"),
        ]
    ],
    audio: OutputPin[
        Annotated[
            Audio,
            Description("Decoded audio blocks, f32 at 48 kHz in the stream's own channel count"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Opus packets in, audio blocks out."""
    decoder = _native.EncodedPacketToAudioBlockDecoder()
    next_start_sample = 0
    next_first_sample_timestamp_ns: "int | None" = None

    def decode_each_packet(frame_ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal next_start_sample, next_first_sample_timestamp_ns
        del frame_ctx
        while (message := encoded_audio.read_message()) is not None:
            upstream_stream_facts = encoded_audio.upstream_stream_facts(
                inbound_link=message.inbound_link
            )
            try:
                block = decoder.decode_one_arriving_message(
                    message.payload.payload,
                    message.timestamp_ns,
                    message.starts_group,
                    message.lost_since_last_read,
                    None if upstream_stream_facts is None else upstream_stream_facts.to_wire(),
                )
            finally:
                _log_codec_notices(decoder.take_notices())
            if block is None:
                continue
            planar_samples = numpy.ascontiguousarray(
                numpy.frombuffer(block.interleaved_sample_bytes, dtype=numpy.float32)
                .reshape(block.sample_count, block.channels)
                .T
            )
            next_start_sample += samples_a_loss_left_out_before(
                block, next_first_sample_timestamp_ns
            )
            audio.write(
                planar_samples,
                block.first_sample_timestamp_ns,
                start_sample=next_start_sample,
                sample_rate=OPUS_SAMPLE_RATE_HZ,
            )
            next_start_sample += block.sample_count
            next_first_sample_timestamp_ns = block.first_sample_timestamp_ns + (
                block.sample_count * NANOSECONDS_PER_SECOND // OPUS_SAMPLE_RATE_HZ
            )

    return decode_each_packet


def samples_a_loss_left_out_before(
    block: _native.DecodedAudioBlock, next_first_sample_timestamp_ns: "int | None"
) -> int:
    """The samples a loss left out between the last published block, whose successor was due
    at `next_first_sample_timestamp_ns`, and `block`, measured from the stamp the decoder
    re-anchored on."""
    if next_first_sample_timestamp_ns is None:
        return 0
    skipped_ns = block.first_sample_timestamp_ns - next_first_sample_timestamp_ns
    return max(0, round(skipped_ns * OPUS_SAMPLE_RATE_HZ / NANOSECONDS_PER_SECOND))
