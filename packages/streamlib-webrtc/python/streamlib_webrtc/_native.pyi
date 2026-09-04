# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `streamlib-webrtc`, as the wheel's Python sees it.

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
    "bring_up_the_transport_stack",
]

def bring_up_the_transport_stack() -> None:
    """Start the tokio runtime and install the TLS provider, once per process.

    What `extension.py:load` calls. Cheap and does no I/O.
    """

@final
class WhipSession:
    """One WHIP publishing session: at most one video and one audio track."""

    def __new__(
        cls, endpoint_url: str, bearer_token: str | None = None
    ) -> WhipSession:
        """Construct without connecting — the first bag is what connects."""

    def connect(
        self, *, video: bool, audio: bool, audio_channels: int | None = None
    ) -> None:
        """Offer the media set the publisher's links settled, and set the answer.

        `audio_channels` reaches the far end as the Opus fmtp's `sprop-stereo`,
        never as the rtpmap's encoding parameter — RFC 7587 §7 fixes that at 2
        for every Opus stream, mono included.
        """

    def write_video_access_unit(
        self, annex_b_access_unit: bytes, timestamp_ns: int
    ) -> None:
        """Send one whole Annex-B access unit.

        Whole, not per NAL unit: the H.264 payloader does its own STAP-A
        aggregation and FU-A fragmentation, and one Sample per NAL would
        advance the RTP clock once per NAL rather than once per picture.
        """

    def write_audio_packet(self, opus_packet: bytes, sample_count: int) -> None:
        """Send one Opus packet, its RTP advance taken from `sample_count`."""

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
    def bitstream(self) -> bytes:
        """The Annex-B access unit, start codes included."""

    @property
    def is_sync_point(self) -> bool: ...
    @property
    def group_index(self) -> int: ...
    @property
    def sequence_index(self) -> int: ...
    @property
    def width(self) -> int:
        """From the SPS the stream carried, cropped as the SPS directs."""

    @property
    def height(self) -> int: ...
    @property
    def timestamp_ns(self) -> int:
        """Monotonic, anchored at the stream's first packet and advanced by the
        RTP clock — so jitter on the wire does not become jitter in the stamps."""

    @property
    def color(self) -> "dict[str, str] | None":
        """The bag's `color` sub-map, or `None` where the VUI described none.

        An axis carrying an H.273 enumerant the bag vocabulary does not model is
        left out rather than guessed at.
        """

@final
class PlayedOpusPacket:
    """One Opus packet off a WHEP stream, described by its own TOC byte."""

    @property
    def bitstream(self) -> bytes: ...
    @property
    def group_index(self) -> int: ...
    @property
    def sequence_index(self) -> int: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int:
        """From the TOC's stereo bit, which is the only honest source: the
        answer's rtpmap says 2 for every Opus stream ever negotiated."""

    @property
    def sample_count(self) -> int: ...
    @property
    def pre_skip(self) -> int:
        """Always 0 — RTP carries no `OpusHead` to state a lookahead."""

    @property
    def timestamp_ns(self) -> int: ...
