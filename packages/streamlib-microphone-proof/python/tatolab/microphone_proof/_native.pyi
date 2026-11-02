# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-microphone-proof`, as the wheel's Python sees it."""

from typing import final

__all__ = [
    "AlsaMicrophoneCapture",
    "CapturedAudioBlockMessage",
    "make_the_calling_thread_realtime",
    "monotonic_now_ns",
]

def make_the_calling_thread_realtime() -> str:
    """Make the calling OS thread realtime the way the engine does; how it ended up scheduled."""

def monotonic_now_ns() -> int:
    """Nanoseconds on the clock this wheel stamps blocks with."""

@final
class CapturedAudioBlockMessage:
    """One captured block, already encoded as its audio-block message."""

    @property
    def payload(self) -> bytes:
        """The msgpack audio-block payload, ready for `outputs.write`."""
    @property
    def first_sample_timestamp_ns(self) -> int: ...
    @property
    def sample_count(self) -> int: ...

@final
class AlsaMicrophoneCapture:
    """One ALSA capture stream, its device thread, and the ring between them."""

    def __new__(
        cls,
        pcm_name: str,
        sample_rate: int,
        channels: int,
        block_frames: int,
        ring_capacity_blocks: int,
    ) -> "AlsaMicrophoneCapture": ...
    def start(self) -> None: ...
    def next_message(self, timeout_ms: int) -> "CapturedAudioBlockMessage | None": ...
    def stop(self) -> None: ...
    @property
    def capture_has_ended(self) -> bool: ...
    @property
    def ring_dropped_block_count(self) -> int: ...
    @property
    def device_overrun_count(self) -> int: ...
    @property
    def captured_block_count(self) -> int: ...
    @property
    def device_thread_priority(self) -> "str | None": ...
    @property
    def failure_that_ended_the_capture(self) -> "str | None": ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int: ...
    @property
    def buffer_frames(self) -> int: ...
    @property
    def period_frames(self) -> int: ...
