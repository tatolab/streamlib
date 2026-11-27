# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-webrtc`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest, the same way
the engine wheel's own stub is. The engine never calls anything here — a
node extension's per-frame work is its own package's Rust, reached from
its own Python.
"""

from collections.abc import Buffer
from typing import final

# pyo3 builds this from what the module registers, so the stub has to carry it
# too or stubtest reports the module as under-described.
__all__ = [
    "WhepPeerConnectionFailed",
    "WhepPlayerSession",
    "WhepPlayerThread",
    "WhepPlayerThreadEnded",
    "WhepSessionNotOpened",
    "WhepSignallingFailed",
    "WhipPeerConnectionFailed",
    "WhipSession",
    "WhipSessionNotOpened",
    "WhipSignallingFailed",
    "monotonic_now_ns",
]

class WhipSessionNotOpened(RuntimeError):
    """A WHIP session did not open; the session already publishing keeps publishing."""

class WhipSignallingFailed(WhipSessionNotOpened):
    """The WHIP endpoint refused the offer or did not answer it."""

class WhipPeerConnectionFailed(WhipSessionNotOpened):
    """The WHIP endpoint answered, and the peer connection to it did not come up."""

class WhepSessionNotOpened(RuntimeError):
    """A WHEP session did not open; the endpoint already playing keeps playing."""

class WhepSignallingFailed(WhepSessionNotOpened):
    """The WHEP endpoint refused the offer or did not answer it."""

class WhepPeerConnectionFailed(WhepSessionNotOpened):
    """The WHEP endpoint answered, and the peer connection to it did not come up."""

class WhepPlayerThreadEnded(RuntimeError):
    """The WHEP player's reading thread has ended, so no session opens on it."""

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
        """Offer the media set the publisher's links settled, and set the answer; a session
        that does not open raises `WhipSessionNotOpened`.

        `audio_channels` reaches the far end as the Opus fmtp's `sprop-stereo`,
        never as the rtpmap's encoding parameter — RFC 7587 §7 fixes that at 2
        for every Opus stream, mono included.
        """

    def write_video_access_unit(self, annex_b_access_unit: Buffer, timestamp_ns: int) -> None:
        """Send one Annex-B access unit on the RTP clock `timestamp_ns` places it.

        Whole, not per NAL unit: the H.264 payloader does its own STAP-A
        aggregation and FU-A fragmentation, and one Sample per NAL would
        advance the RTP clock once per NAL rather than once per picture.

        The RTP clock advances by the gap to the *previous* frame, because the
        payloader applies a sample's duration to the frame after it. The
        stream's rate is exact and its numbering trails real time by one frame,
        so video presents roughly one frame interval later than audio — about
        33 ms at 30 fps. An empty message raises `ValueError`.
        """

    def write_audio_opus_packet(self, opus_packet: Buffer) -> None:
        """Send one Opus packet, its RTP advance taken from the packet's own TOC."""

    def close(self) -> None:
        """Close the peer connection and DELETE the session, both bounded."""

    @property
    def is_connected(self) -> bool: ...

@final
class WhepPlayerThread:
    """A thread of this wheel's Rust playing the newest endpoint opened on it, writing
    `encoded_video` and `encoded_audio` through a module host table. Let go, it stops.

    The thread reconnects with backoff when a session ends, states each port's stream facts
    before the first frame they describe and again when the stream changes, and holds no Python
    while it runs.
    """

    def __new__(cls, module_host_table: object) -> WhepPlayerThread:
        """Start the thread on the ports `module_host_table` lends; it writes nothing until an
        endpoint opens on it.

        `module_host_table` is what `ctx.open_module_host_table(output_ports=
        ["encoded_video", "encoded_audio"])` returns.
        """

    def open_session(
        self, endpoint_url: str, bearer_token: str | None = None
    ) -> WhepPlayerSession:
        """Play `endpoint_url` from then on, in place of the endpoint playing.

        While another endpoint plays it is dialled here, and a failure raises a
        `WhepSessionNotOpened` of its kind while the one playing keeps playing. With none
        playing, the thread dials it, and redials it with backoff.
        """

@final
class WhepPlayerSession:
    """An endpoint open on a `WhepPlayerThread`, closed by `close()` or when let go."""

    def close(self) -> None:
        """Close the endpoint: the thread closes its session, and plays the endpoint opened
        before it if that one is still open."""
