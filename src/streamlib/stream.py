"""
Stream orchestrator for coordinating sources, compositor, and sinks.

This module provides the Stream class that manages the main execution loop,
driven by a Clock.
"""

import asyncio
from typing import Optional
from .base import StreamSource, StreamSink, Compositor
from .timing import Clock, SoftwareClock


class Stream:
    """
    Orchestrates sources, compositor, sinks, and clock synchronization.

    The Stream is the central execution loop that:
    - Manages clock (own or synchronized to upstream)
    - Drives compositor at clock rate
    - Coordinates source → compositor → sink pipeline
    - Handles dynamic clock switching (future Phase 4)

    Example:
        stream = Stream(
            source=WebcamSource(),
            compositor=DefaultCompositor(),
            sink=DisplaySink(),
            fps=60
        )
        await stream.run()
    """

    def __init__(
        self,
        source: Optional[StreamSource] = None,
        compositor: Optional[Compositor] = None,
        sink: Optional[StreamSink] = None,
        clock: Optional[Clock] = None,
        fps: float = 60
    ):
        """
        Initialize stream.

        Args:
            source: Optional source to read frames from
            compositor: Optional compositor to process frames
            sink: Sink to write frames to (required)
            clock: Optional clock (defaults to SoftwareClock)
            fps: Target FPS if using default SoftwareClock
        """
        self.source = source
        self.compositor = compositor
        self.sink = sink

        # Clock management
        self.clock = clock or SoftwareClock(fps=fps)
        self.clock_source = "self"

        # State
        self._running = False

    async def run(self) -> None:
        """
        Main execution loop driven by clock.

        Runs until stop() is called or an exception occurs.
        """
        if not self.sink:
            raise ValueError("Stream requires a sink")

        # Start components
        await self.sink.start()
        if self.source:
            await self.source.start()

        self._running = True

        try:
            async for tick in self.clock.tick():
                if not self._running:
                    break

                # Check if sink wants to quit (e.g., user pressed 'q')
                if hasattr(self.sink, 'should_quit') and self.sink.should_quit():
                    break

                # Get input frame if source exists
                input_frame = None
                if self.source:
                    try:
                        input_frame = await self.source.next_frame()
                    except EOFError:
                        # Source exhausted
                        break

                # Composite if compositor exists
                if self.compositor:
                    frame = await self.compositor.composite(input_frame, tick)
                elif input_frame:
                    # No compositor, pass through input frame
                    frame = input_frame
                else:
                    # No compositor and no input frame
                    continue

                # Output frame
                await self.sink.write_frame(frame)

        finally:
            # Cleanup
            await self.sink.stop()
            if self.source:
                await self.source.stop()

    def stop(self) -> None:
        """Stop the stream."""
        self._running = False
        if isinstance(self.clock, SoftwareClock):
            self.clock.stop()

    async def run_for_duration(self, duration: float) -> None:
        """
        Run stream for a specified duration.

        Args:
            duration: Duration in seconds
        """
        async def stop_after_duration():
            await asyncio.sleep(duration)
            self.stop()

        # Run both the stream and the timeout, first one to complete wins
        await asyncio.gather(
            self.run(),
            stop_after_duration(),
            return_exceptions=True
        )

    async def run_for_frames(self, num_frames: int) -> None:
        """
        Run stream for a specified number of frames.

        Args:
            num_frames: Number of frames to process
        """
        if not self.sink:
            raise ValueError("Stream requires a sink")

        # Start components
        await self.sink.start()
        if self.source:
            await self.source.start()

        try:
            frame_count = 0
            async for tick in self.clock.tick():
                if frame_count >= num_frames:
                    break

                # Get input frame if source exists
                input_frame = None
                if self.source:
                    try:
                        input_frame = await self.source.next_frame()
                    except EOFError:
                        break

                # Composite if compositor exists
                if self.compositor:
                    frame = await self.compositor.composite(input_frame, tick)
                elif input_frame:
                    frame = input_frame
                else:
                    continue

                # Output frame
                await self.sink.write_frame(frame)
                frame_count += 1

        finally:
            # Cleanup
            await self.sink.stop()
            if self.source:
                await self.source.stop()
