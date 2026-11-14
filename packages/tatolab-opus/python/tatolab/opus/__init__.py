# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Opus encode and decode for StreamLib: `OpusEncoder` and `OpusDecoder`.

An extension wheel that links no engine crate; each node runs in its own
processor interpreter like any other Python node, and is defined here so its
type is `tatolab.opus:OpusEncoder` and `tatolab.opus:OpusDecoder`.

Both carry bare Opus packets, one per message, each opening its own group and
stamped on the message itself. The codec state is
this wheel's Rust; what lives here is the port surface, the stream facts and
the log lines, because a helper process's Rust has no `tracing` subscriber.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated, Literal

import numpy

from tatolab.stream import (
    AudioWindowContract,
    Channels,
    Input,
    Output,
    Packets,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.stream_facts import PacketsStreamFacts

from . import _native

__all__ = ["OpusDecoder", "OpusEncoder", "OpusEncoderConfig"]

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


@dataclasses.dataclass
class OpusEncoderConfig:
    """What an `OpusEncoder` is configured with; `{}` is a legal config."""

    bitrate_bps: Annotated[
        "int | None",
        "Target bitrate in bits per second. Absent, libopus picks its own from the "
        "sample rate and channel count.",
    ] = None
    application: Annotated[
        OpusEncoderApplication,
        "Which libopus tuning to encode for: `audio` for broadcast and high fidelity, "
        "`voip` for speech, `lowdelay` for the lowest latency at a cost in quality.",
    ] = "audio"


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
)
@node.process(scheduling="high")
class OpusEncoder:
    """Audio windows in, Opus packets out."""

    def __init__(self, config: OpusEncoderConfig) -> None:
        bitrate_bps = config.bitrate_bps
        if bitrate_bps is not None and (
            isinstance(bitrate_bps, bool)
            or not isinstance(bitrate_bps, int)
            or not 0 < bitrate_bps <= _HIGHEST_BITRATE_BPS_LIBOPUS_TAKES
        ):
            raise ValueError(
                f"OpusEncoder: `bitrate_bps` is {bitrate_bps!r}; it takes a count of bits per "
                f"second from 1 to {_HIGHEST_BITRATE_BPS_LIBOPUS_TAKES}, or nothing for "
                "libopus's own choice"
            )
        self._encoder = _native.AudioWindowToEncodedPacketEncoder(
            config.bitrate_bps, config.application
        )
        self._stream_facts_owed_to_the_port: "_native.OpusEncoderPortStreamFacts | None" = None

    audio = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=OPUS_SAMPLE_RATE_HZ,
            dtype="f32",
            window_size=SAMPLES_IN_ONE_OPUS_WINDOW,
            hop=SAMPLES_IN_ONE_OPUS_WINDOW,
        ),
        description=(
            "Audio to encode, resampled and framed to 20 ms at Opus's own rate in the "
            "source's own channel count"
        ),
    )
    encoded_audio = Output(
        Packets, codec=OPUS_RFC6381_CODEC_STRING, description="Opus packets, one per message"
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (window := ctx.inputs.read(self.audio)) is not None:
            interleaved_samples = numpy.ascontiguousarray(numpy.from_dlpack(window).T)
            try:
                opus_packet_bytes = self._encoder.encode_one_window(
                    interleaved_samples.tobytes(),
                    window.sample_rate,
                    window.channel_count,
                    window.samples_per_channel,
                    window.dtype,
                )
            finally:
                _log_codec_notices(self._encoder.take_notices())
            if opus_packet_bytes is None:
                continue
            owed = self._encoder.take_stream_facts_owed_to_the_port()
            if owed is not None:
                self._stream_facts_owed_to_the_port = owed
            # Set ahead of the first packet it describes, and kept owed until
            # the port takes it.
            if self._stream_facts_owed_to_the_port is not None:
                ctx.outputs.set_stream_facts(
                    ENCODED_AUDIO_PORT,
                    PacketsStreamFacts(
                        rfc6381_codec=OPUS_RFC6381_CODEC_STRING,
                        sample_rate=self._stream_facts_owed_to_the_port.sample_rate,
                        channel_count=self._stream_facts_owed_to_the_port.channel_count,
                        codec_configuration_hex=self._stream_facts_owed_to_the_port.codec_configuration_hex,
                    ),
                )
                self._stream_facts_owed_to_the_port = None
            # Stamped with the instant the packet's first sample was captured,
            # never the moment it was published.
            ctx.outputs.write(
                self.encoded_audio,
                opus_packet_bytes,
                timestamp_ns=window.timestamp_ns,
                starts_group=True,
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        log.info(
            "OpusEncoder: teardown",
            packets_encoded=self._encoder.packets_encoded,
        )


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
)
@node.process(scheduling="high")
class OpusDecoder:
    """Opus packets in, audio blocks out."""

    def __init__(self) -> None:
        self._decoder = _native.EncodedPacketToAudioBlockDecoder()
        self._next_start_sample = 0
        self._next_first_sample_timestamp_ns: int | None = None

    encoded_audio = Input(
        Packets,
        delivery="ordered",
        requires_codec=(OPUS_RFC6381_CODEC_STRING,),
        description="Opus packets to decode",
    )
    audio = Output(
        Channels,
        rate=OPUS_SAMPLE_RATE_HZ,
        dtype="f32",
        description="Decoded audio blocks at 48 kHz in the stream's own channel count",
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (message := ctx.inputs.read_message(ENCODED_AUDIO_PORT)) is not None:
            upstream_stream_facts = ctx.inputs.upstream_stream_facts(
                ENCODED_AUDIO_PORT, inbound_link=message.inbound_link
            )
            try:
                block = self._decoder.decode_one_arriving_message(
                    message.payload,
                    message.timestamp_ns,
                    message.starts_group,
                    message.lost_since_last_read,
                    None if upstream_stream_facts is None else upstream_stream_facts.to_wire(),
                )
            finally:
                _log_codec_notices(self._decoder.take_notices())
            if block is None:
                continue
            planar_samples = numpy.ascontiguousarray(
                numpy.frombuffer(block.interleaved_sample_bytes, dtype=numpy.float32)
                .reshape(block.sample_count, block.channels)
                .T
            )
            self._next_start_sample += self._samples_skipped_before(block)
            ctx.outputs.write(
                self.audio,
                planar_samples,
                timestamp_ns=block.first_sample_timestamp_ns,
                start_sample=self._next_start_sample,
            )
            self._next_start_sample += block.sample_count
            self._next_first_sample_timestamp_ns = block.first_sample_timestamp_ns + (
                block.sample_count * NANOSECONDS_PER_SECOND // OPUS_SAMPLE_RATE_HZ
            )

    def _samples_skipped_before(self, block: _native.DecodedAudioBlock) -> int:
        """The samples a loss left out between the last published block and
        `block`, measured from the stamp the decoder re-anchored on."""
        if self._next_first_sample_timestamp_ns is None:
            return 0
        skipped_ns = block.first_sample_timestamp_ns - self._next_first_sample_timestamp_ns
        return max(0, round(skipped_ns * OPUS_SAMPLE_RATE_HZ / NANOSECONDS_PER_SECOND))

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        log.info(
            "OpusDecoder: teardown",
            blocks_published=self._decoder.blocks_published,
            packets_lost_on_the_link=self._decoder.packets_lost_on_the_link,
        )
