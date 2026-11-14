# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-h265`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest.
"""

from typing import final

__all__ = ["H265DecoderThread", "H265EncoderThread"]

@final
class H265EncoderThread:
    """The thread that reads `video`, encodes on the owned device and writes
    `encoded_video`, started over the module host table the node lent both
    ports through."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        width: int | None = None,
        height: int | None = None,
        fps: int | None = None,
        bitrate_bps: int | None = None,
        keyframe_interval_seconds: int | None = None,
        effort_level: int | None = None,
    ) -> H265EncoderThread:
        """Start encoding the frames `video` receives onto `encoded_video`."""

    def stop_and_join(self, timeout_seconds: float) -> bool:
        """Ask the thread to stop and wait up to `timeout_seconds`: `True` when it ended."""

@final
class H265DecoderThread:
    """The thread that reads `encoded_video`, decodes on the owned device and
    writes `video`, started over the module host table the node lent both
    ports through."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        max_width: int | None = None,
        max_height: int | None = None,
    ) -> H265DecoderThread:
        """Start decoding the media frames `encoded_video` receives onto `video`."""

    def stop_and_join(self, timeout_seconds: float) -> bool:
        """Ask the thread to stop and wait up to `timeout_seconds`: `True` when it ended."""
