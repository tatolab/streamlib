# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-h264`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest.
"""

from typing import final

__all__ = [
    "H264DecodeSession",
    "H264DecoderThread",
    "H264EncodeSession",
    "H264EncoderThread",
    "VideoCodingSessionRefused",
    "VideoCodingSessionUnanswered",
    "VideoCodingThreadEnded",
]

class VideoCodingSessionRefused(RuntimeError):
    """The codec refused a session: its settings, the device, or the video codec host. The
    sessions already open keep coding."""

class VideoCodingThreadEnded(RuntimeError):
    """The coding thread has ended, so no session opens on it."""

class VideoCodingSessionUnanswered(TimeoutError):
    """The coding thread did not open a session in time; it closes the session if it opens
    after."""

@final
class H264EncoderThread:
    """The thread that reads `video`, encodes on the owned device with the newest session
    opened on it and writes `encoded_video`, started over the module host table the node lent
    both ports through. Let go, it stops."""

    def __new__(cls, module_host_table: object) -> H264EncoderThread:
        """Start reading `video` and writing `encoded_video`; frames are drained until a
        session opens."""

    def open_session(
        self,
        *,
        width: int | None = None,
        height: int | None = None,
        fps: int | None = None,
        bitrate_bps: int | None = None,
        keyframe_interval_seconds: int | None = None,
        effort_level: int | None = None,
    ) -> H264EncodeSession:
        """Open a session of these settings beside the ones open, and encode every frame with
        it from then on."""

@final
class H264EncodeSession:
    """A session open on an `H264EncoderThread`, closed by `close()` or when let go."""

    def close(self) -> None:
        """Close the session: the thread drops it before its next frame."""

@final
class H264DecoderThread:
    """The thread that reads `encoded_video`, decodes on the owned device with the newest
    session opened on it from its first group start and writes `video`, started over the
    module host table the node lent both ports through. Let go, it stops."""

    def __new__(cls, module_host_table: object) -> H264DecoderThread:
        """Start reading `encoded_video` and writing `video`; access units are drained until a
        session opens."""

    def open_session(
        self, *, max_width: int | None = None, max_height: int | None = None
    ) -> H264DecodeSession:
        """Open a session of these settings beside the ones open; the thread moves to it at
        the next group start."""

@final
class H264DecodeSession:
    """A session open on an `H264DecoderThread`, closed by `close()` or when let go."""

    def close(self) -> None:
        """Close the session: the thread drops it before its next media frame."""
