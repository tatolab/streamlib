# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-microphone`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest.
"""

from typing import final

__all__ = ["MicrophoneCapture"]

@final
class MicrophoneCapture:
    """One node's microphone, opened through the module host table and
    published on the node's lent `audio` port."""

    def __new__(cls, module_host_table: object, device_id: str | None = None) -> MicrophoneCapture: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int: ...
    @property
    def sample_format(self) -> str: ...
    @property
    def published_block_count(self) -> int: ...
    @property
    def dropped_block_count(self) -> int: ...
    @property
    def failure_that_ended_the_capture(self) -> str | None: ...
    def start(self) -> None: ...
    def stop(self) -> None: ...
