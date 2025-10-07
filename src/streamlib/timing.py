"""
Timing infrastructure for precise time synchronization.

This module provides:
- PTP (IEEE 1588) client for sub-millisecond synchronization
- Frame timing utilities
- Multi-stream temporal alignment
"""

import time
import asyncio
from typing import Optional, Dict, List
from dataclasses import dataclass
import numpy as np
from .base import TimestampedFrame


class FrameTimer:
    """
    Utility for managing frame timing and pacing.

    Ensures frames are produced/consumed at the target FPS.
    """

    def __init__(self, fps: int = 30):
        """
        Initialize frame timer.

        Args:
            fps: Target frames per second
        """
        self.fps = fps
        self.frame_duration = 1.0 / fps
        self.last_frame_time = 0.0
        self.frame_count = 0

    async def wait_for_next_frame(self) -> None:
        """
        Wait until it's time for the next frame.

        This maintains consistent frame timing by sleeping for the
        remaining time until the next frame should be produced.
        """
        current_time = time.time()
        if self.last_frame_time == 0.0:
            self.last_frame_time = current_time
            return

        elapsed = current_time - self.last_frame_time
        sleep_time = self.frame_duration - elapsed

        if sleep_time > 0:
            await asyncio.sleep(sleep_time)

        self.last_frame_time = time.time()
        self.frame_count += 1

    def get_current_timestamp(self) -> float:
        """Get current wall clock timestamp."""
        return time.time()

    def get_frame_number(self) -> int:
        """Get current frame number."""
        return self.frame_count


class PTPClient:
    """
    PTP (IEEE 1588) client for precision time synchronization.

    This enables sub-millisecond time synchronization across multiple
    devices/sources for applications like multi-camera 3D tracking.

    Note: This is a simplified implementation. For production use,
    consider using a full PTP daemon like linuxptp.
    """

    def __init__(self, master_clock: Optional[str] = None):
        """
        Initialize PTP client.

        Args:
            master_clock: Optional IP address of PTP master clock.
                         If None, system clock is used.
        """
        self.master_clock = master_clock
        self.offset = 0.0  # Offset from master in seconds
        self.synced = False
        self._sync_task = None

    async def start(self) -> None:
        """Start PTP synchronization."""
        if self.master_clock:
            self._sync_task = asyncio.create_task(self._sync_loop())
        else:
            # No master clock, just use system time
            self.synced = True

    async def stop(self) -> None:
        """Stop PTP synchronization."""
        if self._sync_task:
            self._sync_task.cancel()
            try:
                await self._sync_task
            except asyncio.CancelledError:
                pass

    async def _sync_loop(self) -> None:
        """
        Periodic synchronization with master clock.

        This is a simplified sync loop. A full implementation would
        use the PTP protocol with hardware timestamping.
        """
        while True:
            try:
                # TODO: Implement actual PTP sync protocol
                # For now, this is a placeholder
                await asyncio.sleep(1.0)
            except asyncio.CancelledError:
                break

    def get_ptp_time(self) -> float:
        """
        Get current PTP synchronized time.

        Returns:
            Current time in seconds (with sub-millisecond precision)
        """
        return time.time() + self.offset

    def is_synced(self) -> bool:
        """Check if PTP is synchronized."""
        return self.synced


@dataclass
class SyncedFrame:
    """
    Container for temporally aligned frames from multiple sources.
    """
    frames: Dict[str, TimestampedFrame]
    sync_time: float  # The time at which all frames are aligned
    max_offset: float  # Maximum time offset between frames


class MultiStreamSynchronizer:
    """
    Synchronize frames from multiple sources based on timestamps.

    This is critical for applications like:
    - Multi-camera 3D tracking
    - Distributed ML processing (waiting for all results)
    - Synchronized playback from multiple sources
    """

    def __init__(
        self,
        max_offset: float = 0.033,  # ~1 frame at 30 FPS
        buffer_size: int = 60  # Buffer up to 60 frames per source
    ):
        """
        Initialize synchronizer.

        Args:
            max_offset: Maximum allowed time offset between frames (seconds)
            buffer_size: Maximum number of frames to buffer per source
        """
        self.max_offset = max_offset
        self.buffer_size = buffer_size
        self.buffers: Dict[str, List[TimestampedFrame]] = {}
        self.source_ids: List[str] = []

    def add_source(self, source_id: str) -> None:
        """
        Add a source to synchronize.

        Args:
            source_id: Unique identifier for this source
        """
        if source_id not in self.source_ids:
            self.source_ids.append(source_id)
            self.buffers[source_id] = []

    def add_frame(self, source_id: str, frame: TimestampedFrame) -> None:
        """
        Add a frame from a source to the synchronization buffer.

        Args:
            source_id: Source that produced this frame
            frame: The timestamped frame
        """
        if source_id not in self.buffers:
            raise ValueError(f"Unknown source: {source_id}")

        buffer = self.buffers[source_id]
        buffer.append(frame)

        # Trim buffer if too large
        if len(buffer) > self.buffer_size:
            buffer.pop(0)

    def get_synced_frames(self) -> Optional[SyncedFrame]:
        """
        Get temporally aligned frames from all sources.

        Returns:
            SyncedFrame if all sources have frames within max_offset,
            None otherwise
        """
        if not all(self.buffers.values()):
            # Not all sources have frames yet
            return None

        # Find the latest minimum timestamp across all sources
        min_times = [
            min(frame.ptp_time or frame.timestamp for frame in buffer)
            for buffer in self.buffers.values()
        ]
        sync_time = max(min_times)

        # Find closest frame to sync_time for each source
        synced = {}
        offsets = []

        for source_id in self.source_ids:
            buffer = self.buffers[source_id]

            # Find frame closest to sync_time
            closest = min(
                buffer,
                key=lambda f: abs((f.ptp_time or f.timestamp) - sync_time)
            )

            offset = abs((closest.ptp_time or closest.timestamp) - sync_time)
            if offset > self.max_offset:
                # Frames are too far apart
                return None

            synced[source_id] = closest
            offsets.append(offset)

            # Remove frames older than the selected one
            self.buffers[source_id] = [
                f for f in buffer
                if (f.ptp_time or f.timestamp) >= (closest.ptp_time or closest.timestamp)
            ]

        return SyncedFrame(
            frames=synced,
            sync_time=sync_time,
            max_offset=max(offsets)
        )

    def clear_buffers(self) -> None:
        """Clear all frame buffers."""
        for buffer in self.buffers.values():
            buffer.clear()


def estimate_fps(timestamps: List[float]) -> float:
    """
    Estimate FPS from a list of frame timestamps.

    Args:
        timestamps: List of frame timestamps in seconds

    Returns:
        Estimated frames per second
    """
    if len(timestamps) < 2:
        return 0.0

    diffs = np.diff(timestamps)
    avg_diff = np.mean(diffs)

    if avg_diff <= 0:
        return 0.0

    return 1.0 / avg_diff


def align_timestamps(
    timestamps: List[float],
    reference_time: float
) -> List[float]:
    """
    Align timestamps to a reference time.

    Args:
        timestamps: List of timestamps to align
        reference_time: Reference timestamp

    Returns:
        Aligned timestamps
    """
    offset = reference_time - timestamps[0]
    return [t + offset for t in timestamps]
