"""
Clock abstraction for timing and synchronization.

Supports multiple clock sources:
- SoftwareClock: Free-running software timer (bathtub mode)
- PTPClock: IEEE 1588 Precision Time Protocol (stub for Phase 4)
- GenlockClock: SDI hardware sync (stub for Phase 4)

Clocks are swappable to support different sync sources:
    if genlock_signal_present:
        clock = GenlockClock(sdi_port)
    elif ptp_available:
        clock = PTPClock(ptp_client)
    else:
        clock = SoftwareClock(fps=60)
"""

import asyncio
import time
from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Optional, Any


@dataclass
class TimedTick:
    """
    Clock tick with timing information.

    Ticks are signals to process, not data carriers. Actors receive ticks
    and read latest data from ring buffers.

    Attributes:
        timestamp: Absolute time in seconds (since epoch or relative)
        frame_number: Monotonic frame counter (starts at 0)
        clock_id: Identifier for clock source (e.g., 'software', 'ptp:0')
    """
    timestamp: float
    frame_number: int
    clock_id: str


class Clock(ABC):
    """
    Abstract clock interface.

    All clocks generate ticks at a specific rate and provide timing info.
    """

    @abstractmethod
    async def next_tick(self) -> TimedTick:
        """
        Wait for and return the next tick.

        Returns:
            TimedTick with timing information

        Note: This is async and will sleep until the next tick time.
        """
        pass

    @abstractmethod
    def get_fps(self) -> float:
        """
        Get nominal frame rate (ticks per second).

        Returns:
            FPS as a float
        """
        pass

    @abstractmethod
    def get_clock_id(self) -> str:
        """
        Get clock identifier.

        Returns:
            String identifying clock source
        """
        pass


class SoftwareClock(Clock):
    """
    Free-running software clock (bathtub mode).

    Generates ticks at a fixed rate using asyncio.sleep(). Suitable for:
    - Local development
    - Isolated actors (no network sync needed)
    - Testing

    Not suitable for:
    - Multi-device synchronization (use PTPClock)
    - Hardware sync (use GenlockClock)
    """

    def __init__(self, fps: float = 60.0, clock_id: Optional[str] = None):
        """
        Initialize software clock.

        Args:
            fps: Frames per second (ticks per second)
            clock_id: Optional clock identifier (default: 'software')
        """
        if fps <= 0:
            raise ValueError(f"FPS must be positive, got {fps}")

        self.fps = fps
        self.period = 1.0 / fps
        self._clock_id = clock_id or 'software'
        self.frame_number = 0
        self.start_time = time.monotonic()

    async def next_tick(self) -> TimedTick:
        """
        Generate next tick at fixed rate.

        Returns:
            TimedTick with current timing info

        Note: Uses time.monotonic() for timing to avoid issues with
        system clock adjustments.
        """
        # Calculate target time for this frame
        target_time = self.start_time + (self.frame_number * self.period)
        now = time.monotonic()
        sleep_time = target_time - now

        # Sleep until target time (if not already late)
        if sleep_time > 0:
            await asyncio.sleep(sleep_time)

        tick = TimedTick(
            timestamp=time.time(),  # Absolute time
            frame_number=self.frame_number,
            clock_id=self._clock_id
        )

        self.frame_number += 1
        return tick

    def get_fps(self) -> float:
        """Get nominal FPS."""
        return self.fps

    def get_clock_id(self) -> str:
        """Get clock identifier."""
        return self._clock_id

    def reset(self) -> None:
        """Reset clock to frame 0."""
        self.frame_number = 0
        self.start_time = time.monotonic()


class PTPClock(Clock):
    """
    IEEE 1588 Precision Time Protocol clock (stub for Phase 4).

    PTP provides microsecond-accurate synchronization across network devices.
    Used in SMPTE ST 2110 professional broadcast environments.

    This is a stub implementation. Real implementation in Phase 4 will:
    - Use linuxptp or similar PTP client
    - Sync to PTP grandmaster clock
    - Provide < 1μs accuracy
    - Support multiple PTP domains

    For now, falls back to software timing.
    """

    def __init__(self, ptp_client: Optional[Any] = None, fps: float = 60.0):
        """
        Initialize PTP clock.

        Args:
            ptp_client: PTP client instance (stub, not implemented)
            fps: Frames per second

        Note: Currently falls back to software timing.
        """
        self.ptp_client = ptp_client
        self.fps = fps
        self.period = 1.0 / fps
        self.frame_number = 0

        # Stub: Use software clock as fallback
        self._fallback = SoftwareClock(fps=fps, clock_id=f'ptp-stub')
        print(f"[PTPClock] Warning: PTP not implemented, using software fallback")

    async def next_tick(self) -> TimedTick:
        """
        Generate tick synced to PTP (stub).

        TODO Phase 4: Implement real PTP synchronization:
        - Get PTP time from client
        - Align tick to frame boundary
        - Sleep until next boundary
        """
        return await self._fallback.next_tick()

    def get_fps(self) -> float:
        """Get nominal FPS."""
        return self.fps

    def get_clock_id(self) -> str:
        """Get clock identifier."""
        domain = getattr(self.ptp_client, 'domain', 0) if self.ptp_client else 0
        return f'ptp:{domain}'


class GenlockClock(Clock):
    """
    SDI hardware sync clock (genlock signal) - stub for Phase 4.

    Genlock provides hardware sync for SDI devices (professional video equipment).
    The genlock signal is a reference pulse (typically black burst or tri-level sync)
    that all devices sync to.

    Different from PTP:
    - PTP: Network-based sync (IEEE 1588)
    - Genlock: Hardware sync pulse on SDI/BNC connector

    This is a stub implementation. Real implementation in Phase 4 will:
    - Interface with SDI hardware (e.g., Blackmagic DeckLink)
    - Wait for hardware pulse
    - Generate ticks aligned to pulse

    For now, falls back to software timing.
    """

    def __init__(self, sdi_device: Optional[Any] = None):
        """
        Initialize genlock clock.

        Args:
            sdi_device: SDI device instance (stub, not implemented)

        Note: Currently falls back to software timing.
        """
        self.sdi_device = sdi_device
        self.frame_number = 0

        # Stub: Assume 60fps, use software clock as fallback
        fps = 60.0  # Would be detected from hardware
        self._fallback = SoftwareClock(fps=fps, clock_id=f'genlock-stub')
        print(f"[GenlockClock] Warning: Genlock not implemented, using software fallback")

    async def next_tick(self) -> TimedTick:
        """
        Wait for genlock pulse (stub).

        TODO Phase 4: Implement real hardware sync:
        - Wait for hardware pulse from SDI device
        - Generate tick when pulse arrives
        - Handle frame rate detection
        """
        return await self._fallback.next_tick()

    def get_fps(self) -> float:
        """
        Get detected FPS from hardware.

        TODO Phase 4: Detect from hardware.
        """
        return self._fallback.get_fps()

    def get_clock_id(self) -> str:
        """Get clock identifier."""
        port = getattr(self.sdi_device, 'port', 0) if self.sdi_device else 0
        return f'genlock:{port}'
