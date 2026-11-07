# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Opus encode and decode for StreamLib: `OpusEncoder` and `OpusDecoder`.

An extension wheel that links no engine crate; each node runs in its own
processor interpreter like any other Python node, and is defined here so its
type is `tatolab.opus:OpusEncoder` and `tatolab.opus:OpusDecoder`.

Both carry media frames — hang's legacy container, a QUIC varint timestamp in
microseconds and then one Opus packet, each opening its own group — the layout
the moq-dev players and `tatolab-webrtc` read and write. The codec state is
this wheel's Rust; what lives here is the port surface, the catalog entry and
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
from tatolab.stream.catalog import AudioEntry

from . import _native

__all__ = ["OpusDecoder", "OpusEncoder", "OpusEncoderConfig"]

ENCODED_AUDIO_PORT = "encoded_audio"

#: Opus's own clock, which the encoder's window contract resamples to.
OPUS_SAMPLE_RATE_HZ = 48_000
NANOSECONDS_PER_SECOND = 1_000_000_000
#: One 20 ms Opus frame at 48 kHz, in per-channel samples.
SAMPLES_IN_ONE_OPUS_WINDOW = 960
OPUS_CATALOG_CODEC_STRING = "opus"
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


@node(
    scheduling="high",
    description=(
        "Encodes 20 ms windows of audio to Opus via libopus.\n\n"
        "`audio` declares `AudioWindowContract(sample_rate=48000, dtype=\"f32\", "
        "window_size=960, hop=960)` and states no channel count, so the engine resamples "
        "to Opus's own clock, converts to `f32` and frames into 20 ms windows while the "
        "count follows whatever the source publishes. One to eight channels; more is "
        "refused by name. `encoded_audio` publishes one media frame per Opus packet — a "
        "QUIC varint timestamp in microseconds, then the packet — each one opening its own "
        "group. The port's catalog entry is an `AudioEntry` — `opus`, 48 000 Hz, the "
        "channel count — carrying the `OpusHead` hex-encoded in `description`, so a "
        "decoder learns the channel count and the encoder's lookahead before the first "
        "packet.\n\n"
        "The libopus encoder is minted from the first window's channel count — one or two "
        "channels as a single stream, three to eight as a multistream under channel "
        "mapping family 1 — and re-mints when the source's count changes, replacing the "
        "port's catalog entry with one naming the new count. In-band FEC and DTX are off "
        "and are not knobs."
    ),
)
class OpusEncoder:
    """Audio windows in, Opus media frames out."""

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
        self._audio_entry_owed_to_the_port: "_native.OpusEncoderPortAudioEntry | None" = None

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
        Packets, codec=OPUS_CATALOG_CODEC_STRING, description="Opus media frames, one packet each"
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (window := ctx.inputs.read(self.audio)) is not None:
            interleaved_samples = numpy.ascontiguousarray(numpy.from_dlpack(window).T)
            try:
                media_frame = self._encoder.encode_one_window(
                    interleaved_samples.tobytes(),
                    window.sample_rate,
                    window.channel_count,
                    window.samples_per_channel,
                    window.dtype,
                    window.timestamp_ns,
                )
            finally:
                _log_codec_notices(self._encoder.take_notices())
            if media_frame is None:
                continue
            owed = self._encoder.take_audio_entry_owed_to_the_port()
            if owed is not None:
                self._audio_entry_owed_to_the_port = owed
            # Set ahead of the first packet it describes, and kept owed until
            # the port takes it.
            if self._audio_entry_owed_to_the_port is not None:
                ctx.outputs.set_catalog_entry(
                    ENCODED_AUDIO_PORT,
                    AudioEntry(
                        codec=OPUS_CATALOG_CODEC_STRING,
                        sampleRate=self._audio_entry_owed_to_the_port.sample_rate,
                        numberOfChannels=self._audio_entry_owed_to_the_port.number_of_channels,
                        description=self._audio_entry_owed_to_the_port.hex_encoded_opus_identification_header,
                    ),
                )
                self._audio_entry_owed_to_the_port = None
            # Stamped with the instant the packet's first sample was captured,
            # never the moment it was published.
            ctx.outputs.write(
                self.encoded_audio,
                media_frame,
                timestamp_ns=window.timestamp_ns,
                starts_group=True,
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        log.info(
            "OpusEncoder: teardown",
            packets_encoded=self._encoder.packets_encoded,
        )


@node(
    scheduling="high",
    description=(
        "Decodes Opus media frames to audio blocks via libopus.\n\n"
        "`encoded_audio` takes media frames in the layout `OpusEncoder` publishes — a "
        "QUIC varint timestamp, then one Opus packet — and declares no window contract: "
        "an encoded link carries whole packets. The channel count and the encoder's "
        "lookahead come from the `OpusHead` in the upstream port's catalog entry. An audio "
        "entry carrying no `OpusHead` names the channel count; with no entry at all, the "
        "first packet's TOC fixes one or two channels for the run. Either way nothing is "
        "trimmed. A message that is not a media frame, an upstream entry that is not an "
        "`opus` audio entry, an `OpusHead` that does not read, and a `pre_skip` past the "
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
class OpusDecoder:
    """Opus media frames in, audio blocks out."""

    def __init__(self) -> None:
        self._decoder = _native.EncodedPacketToAudioBlockDecoder()
        self._next_start_sample = 0
        self._next_first_sample_timestamp_ns: int | None = None

    encoded_audio = Input(
        Packets,
        delivery="ordered",
        requires_codec=(OPUS_CATALOG_CODEC_STRING,),
        description="Opus media frames to decode",
    )
    audio = Output(
        Channels,
        rate=OPUS_SAMPLE_RATE_HZ,
        dtype="f32",
        description="Decoded audio blocks at 48 kHz in the stream's own channel count",
    )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (message := ctx.inputs.read_message(ENCODED_AUDIO_PORT)) is not None:
            upstream_catalog_entry = ctx.inputs.inbound_link_catalog_entry(
                ENCODED_AUDIO_PORT, inbound_link=message.inbound_link
            )
            try:
                block = self._decoder.decode_one_arriving_message(
                    message.payload,
                    message.timestamp_ns,
                    message.starts_group,
                    message.lost_since_last_read,
                    upstream_catalog_entry,
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
