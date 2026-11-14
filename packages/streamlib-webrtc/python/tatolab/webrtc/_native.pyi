# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-webrtc`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest, the same way
the engine wheel's own stub is. The engine never calls anything here — a
processor extension's per-frame work is its own package's Rust, reached from
its own Python.
"""

from collections.abc import Buffer
from typing import final

# pyo3 builds this from what the module registers, so the stub has to carry it
# too or stubtest reports the module as under-described.
__all__ = [
    "WhepPlayerThread",
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
    """One WHEP playing session on a thread of this wheel's Rust, writing
    `encoded_video` and `encoded_audio` through a module host table.

    The thread connects, reconnects with backoff when the session ends, states
    each port's stream facts before the first frame they describe and again
    when the stream changes, and holds no Python while it runs.
    """

    def __new__(
        cls,
        module_host_table: object,
        endpoint_url: str,
        bearer_token: str | None = None,
    ) -> WhepPlayerThread:
        """Start the thread on the ports `module_host_table` lends.

        `module_host_table` is what `ctx.open_module_host_table(output_ports=
        ["encoded_video", "encoded_audio"])` returns.
        """

    def stop_and_join(self, timeout_seconds: float) -> bool:
        """Ask the thread to stop and wait up to `timeout_seconds` for it.

        False when it is still inside a connect; it closes its session itself
        when that connect returns.
        """
