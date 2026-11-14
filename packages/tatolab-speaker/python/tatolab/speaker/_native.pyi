# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-speaker`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest.
"""

from typing import final

__all__ = ["SpeakerPlayback"]

@final
class SpeakerPlayback:
    """One node's speaker: a playback stream opened through the module host
    table, fed from the node's lent `audio` port."""

    def __new__(
        cls,
        module_host_table: object,
        sample_rate: int,
        channels: int,
        sample_format: str,
        device_id: str | None = None,
    ) -> SpeakerPlayback: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int: ...
    @property
    def sample_format(self) -> str: ...
    @property
    def device_period_in_per_channel_samples(self) -> int | None: ...
    @property
    def played_block_count(self) -> int: ...
    @property
    def refused_block_count(self) -> int: ...
    @property
    def underrun_byte_count(self) -> int: ...
    @property
    def silence_before_playback_began_byte_count(self) -> int: ...
    @property
    def failure_that_ended_the_playback(self) -> str | None: ...
    def start(self) -> None: ...
    def stop(self) -> None: ...
