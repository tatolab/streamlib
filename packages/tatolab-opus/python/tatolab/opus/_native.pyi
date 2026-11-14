# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-opus`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest. The engine
never calls anything here.
"""

from collections.abc import Buffer
from typing import Any, Literal, final

# pyo3 builds this from what the module registers, so the stub carries it too.
__all__ = [
    "AudioWindowToEncodedPacketEncoder",
    "DecodedAudioBlock",
    "EncodedPacketToAudioBlockDecoder",
    "OpusEncoderPortStreamFacts",
]

@final
class AudioWindowToEncodedPacketEncoder:
    """Encodes 20 ms windows of audio into Opus packets."""

    def __new__(
        cls, bitrate_bps: int | None, application: str
    ) -> AudioWindowToEncodedPacketEncoder:
        """Refuses an `application` libopus has no tuning for, naming it."""

    def encode_one_window(
        self,
        interleaved_sample_bytes: Buffer,
        sample_rate: int,
        channels: int,
        sample_count: int,
        dtype: str,
    ) -> bytes | None:
        """One Opus packet, or `None` while the encoder discards a source it
        already refused. A window that is not
        what the port's contract promised raises `ValueError` naming it."""

    def take_stream_facts_owed_to_the_port(self) -> OpusEncoderPortStreamFacts | None:
        """The stream facts the last mint owes the output port, once."""

    def take_notices(self) -> list[tuple[Literal["info", "warn"], str]]:
        """What the encoder did since the last call, for the node to log."""

    @property
    def packets_encoded(self) -> int: ...

@final
class OpusEncoderPortStreamFacts:
    """The Packets stream facts an encoder's output port carries."""

    @property
    def channel_count(self) -> int: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def codec_configuration_hex(self) -> str:
        """The `OpusHead`, hex-encoded."""

@final
class EncodedPacketToAudioBlockDecoder:
    """Decodes Opus packets into audio blocks."""

    def __new__(cls) -> EncodedPacketToAudioBlockDecoder: ...
    def decode_one_arriving_message(
        self,
        payload_bytes: Buffer,
        timestamp_ns: int,
        starts_group: bool,
        lost_since_last_read: int,
        upstream_packets_stream_facts: dict[str, Any] | None,
    ) -> DecodedAudioBlock | None:
        """One arriving message decoded in the format the upstream port's
        stream facts name, in their wire form, or `None` when it is discarded or was all encoder
        priming. What cannot be decoded raises `ValueError` naming it."""

    def take_notices(self) -> list[tuple[Literal["info", "warn"], str]]:
        """What the decoder did since the last call, for the node to log."""

    @property
    def blocks_published(self) -> int: ...
    @property
    def packets_lost_on_the_link(self) -> int: ...

@final
class DecodedAudioBlock:
    """One decoded block: interleaved little-endian `f32` at 48 kHz."""

    @property
    def interleaved_sample_bytes(self) -> bytes: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int: ...
    @property
    def sample_count(self) -> int: ...
    @property
    def first_sample_timestamp_ns(self) -> int: ...
