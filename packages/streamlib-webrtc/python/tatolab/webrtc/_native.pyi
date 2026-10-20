# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-webrtc`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest, the same way
the engine wheel's own stub is. The engine never calls anything here — a
processor extension's per-frame work is its own package's Rust, reached from
its own Python.
"""

from typing import final

# pyo3 builds this from what the module registers, so the stub has to carry it
# too or stubtest reports the module as under-described.
__all__ = [
    "PlayedOpusPacket",
    "PlayedVideoAccessUnit",
    "WhepSession",
    "WhipSession",
    "monotonic_now_ns",
]

def monotonic_now_ns() -> int:
    """Nanoseconds on the clock this wheel stamps media frames with.

    The engine's clock, read here rather than asked of the engine, so a
    reading compares with `tatolab.stream.monotonic_now_ns()` taken on this
    machine.
    """

@final
class WhipSession:
    """One WHIP publishing session: at most one video and one audio track."""

    def __new__(
        cls, endpoint_url: str, bearer_token: str | None = None
    ) -> WhipSession:
        """Construct without connecting — the first media frame is what connects."""

    def connect(
        self, *, video: bool, audio: bool, audio_channels: int | None = None
    ) -> None:
        """Offer the media set the publisher's links settled, and set the answer.

        `audio_channels` reaches the far end as the Opus fmtp's `sprop-stereo`,
        never as the rtpmap's encoding parameter — RFC 7587 §7 fixes that at 2
        for every Opus stream, mono included.
        """

    def write_video_media_frame(self, media_frame: bytes, timestamp_ns: int) -> None:
        """Send the Annex-B access unit `media_frame` carries after its varint
        timestamp, on the RTP clock `timestamp_ns` places it.

        Whole, not per NAL unit: the H.264 payloader does its own STAP-A
        aggregation and FU-A fragmentation, and one Sample per NAL would
        advance the RTP clock once per NAL rather than once per picture.

        The RTP clock advances by the gap to the *previous* frame, because the
        payloader applies a sample's duration to the frame after it. The
        stream's rate is exact and its numbering trails real time by one frame,
        so video presents roughly one frame interval later than audio — about
        33 ms at 30 fps. Bytes that are not a media frame raise `ValueError`.
        """

    def write_audio_media_frame(self, media_frame: bytes) -> None:
        """Send the Opus packet `media_frame` carries after its varint
        timestamp, its RTP advance taken from the packet's own TOC."""

    def close(self) -> None:
        """Close the peer connection and DELETE the session, both bounded."""

    @property
    def is_connected(self) -> bool: ...

@final
class WhepSession:
    """One WHEP playing session, draining into an assembled-media queue."""

    def __new__(
        cls, endpoint_url: str, bearer_token: str | None = None
    ) -> WhepSession: ...
    def connect(self) -> None:
        """Connect and begin draining. Called from the processor's own thread."""

    def next_media(
        self, timeout_ms: int
    ) -> "PlayedVideoAccessUnit | PlayedOpusPacket | None":
        """The next assembled item, or `None` if none arrived in `timeout_ms`.

        The timeout is what lets a reading thread notice it has been asked to
        stop without waiting on a stream that may never send again.
        """

    def close(self) -> None:
        """Close the peer connection and DELETE the session, both bounded."""

    @property
    def is_connected(self) -> bool: ...

@final
class PlayedVideoAccessUnit:
    """One access unit off a WHEP stream, described by the stream itself."""

    @property
    def media_frame(self) -> bytes:
        """The Annex-B access unit, start codes included, behind the QUIC
        varint of `timestamp_ns` in microseconds — hang's legacy container."""

    @property
    def is_sync_point(self) -> bool: ...
    @property
    def codec(self) -> str:
        """`avc1.PPCCLL`, spelled from the profile, constraint flags and level
        of the latest SPS the stream carried."""

    @property
    def coded_width(self) -> int:
        """Width before the conformance crop, from the latest SPS — the
        codec-aligned extent a catalog entry's `codedWidth` means and
        `H264Encoder` writes."""

    @property
    def coded_height(self) -> int: ...
    @property
    def timestamp_ns(self) -> int:
        """Monotonic, anchored at the stream's first packet and advanced by the
        RTP clock — so jitter on the wire does not become jitter in the stamps."""

@final
class PlayedOpusPacket:
    """One Opus packet off a WHEP stream, described by the stream itself."""

    @property
    def media_frame(self) -> bytes:
        """The Opus packet behind the QUIC varint of `timestamp_ns` in
        microseconds — hang's legacy container."""

    @property
    def channels(self) -> int:
        """The stream's declared output channel count, constant for its life.

        From the answer's `sprop-stereo`, falling back to the first packet's
        TOC when the answer states no Opus fmtp. Not the rtpmap, which RFC 7587
        §7 fixes at 2 for every Opus stream ever negotiated; and not the TOC per
        packet, which codes a frame rather than declaring a stream — it flips
        when an encoder codes a mono frame inside a stereo stream.
        """

    @property
    def opus_identification_header_hex(self) -> str:
        """The stream's `OpusHead` (RFC 7845 §5.1), hex-encoded: `channels`,
        mapping family 0, and a pre-skip of 0 because RTP states no encoder
        lookahead."""

    @property
    def timestamp_ns(self) -> int: ...
