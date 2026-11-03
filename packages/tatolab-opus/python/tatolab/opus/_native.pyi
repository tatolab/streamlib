# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-opus`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest. The engine
never calls anything here.
"""

from typing import Any, Literal, final

# pyo3 builds this from what the module registers, so the stub carries it too.
__all__ = [
    "AudioWindowToEncodedPacketEncoder",
    "DecodedAudioBlock",
    "EncodedPacketToAudioBlockDecoder",
    "OpusEncoderPortAudioEntry",
]

@final
class AudioWindowToEncodedPacketEncoder:
    """Encodes 20 ms windows of audio into Opus media frames."""

    def __new__(
        cls, bitrate_bps: int | None, application: str
    ) -> AudioWindowToEncodedPacketEncoder:
        """Refuses an `application` libopus has no tuning for, naming it."""

    def encode_one_window(
        self,
        interleaved_sample_bytes: bytes,
        sample_rate: int,
        channels: int,
        sample_count: int,
        dtype: str,
        first_sample_timestamp_ns: int,
    ) -> bytes | None:
        """One media frame stamped at the window's first sample, or `None` while
        the encoder discards a source it already refused. A window that is not
        what the port's contract promised raises `ValueError` naming it."""

    def take_audio_entry_owed_to_the_port(self) -> OpusEncoderPortAudioEntry | None:
        """The catalog entry the last mint owes the output port, once."""

    def take_notices(self) -> list[tuple[Literal["info", "warn"], str]]:
        """What the encoder did since the last call, for the node to log."""

    @property
    def packets_encoded(self) -> int: ...

@final
class OpusEncoderPortAudioEntry:
    """The audio catalog entry an encoder's output port carries."""

    @property
    def number_of_channels(self) -> int: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def hex_encoded_opus_identification_header(self) -> str:
        """The `OpusHead`, hex-encoded, as the entry's `description`."""

@final
class EncodedPacketToAudioBlockDecoder:
    """Decodes Opus media frames into audio blocks."""

    def __new__(cls) -> EncodedPacketToAudioBlockDecoder: ...
    def decode_one_arriving_message(
        self,
        payload_bytes: bytes,
        timestamp_ns: int,
        starts_group: bool,
        lost_since_last_read: int,
        upstream_catalog_entry: dict[str, Any] | None,
    ) -> DecodedAudioBlock | None:
        """One arriving message decoded in the format the upstream port's
        catalog entry names, or `None` when it is discarded or was all encoder
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
