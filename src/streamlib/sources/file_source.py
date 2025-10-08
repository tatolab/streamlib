"""
FileSource - Read video files using PyAV.

This source reads frames from video files in common formats (MP4, MOV, AVI, etc.)
using PyAV's FFmpeg bindings.
"""

import av
import numpy as np
from typing import Optional
from ..base import StreamSource, TimestampedFrame
from ..plugins import register_source


@register_source('file')
class FileSource(StreamSource):
    """
    Source that reads frames from a video file.

    Uses PyAV to decode video files in any format supported by FFmpeg.
    Supports seeking and provides frame-accurate playback.

    Args:
        path: Path to the video file
        loop: Whether to loop the video when it reaches the end
        start_frame: Frame number to start from (default: 0)
        end_frame: Frame number to end at (default: None = end of file)
        **kwargs: Additional arguments passed to StreamSource

    Example:
        source = FileSource('input.mp4', loop=True)
        await source.start()

        async for frame in source.frames():
            # Process frame
            pass

        await source.stop()
    """

    def __init__(
        self,
        path: str,
        loop: bool = False,
        start_frame: int = 0,
        end_frame: Optional[int] = None,
        **kwargs
    ):
        # Open container to get video properties
        container = av.open(path)
        video_stream = container.streams.video[0]

        # Extract properties for base class
        width = video_stream.width
        height = video_stream.height
        fps = int(video_stream.average_rate) if video_stream.average_rate else 30

        # Close container, will reopen in start()
        container.close()

        super().__init__(width=width, height=height, fps=fps, **kwargs)

        self.path = path
        self.loop = loop
        self.start_frame = start_frame
        self.end_frame = end_frame

        self._container: Optional[av.container.InputContainer] = None
        self._stream: Optional[av.video.stream.VideoStream] = None
        self._current_frame = 0

    async def start(self) -> None:
        """Open the video file and seek to start frame."""
        self._container = av.open(self.path)
        self._stream = self._container.streams.video[0]
        self._current_frame = 0

        # Seek to start frame if specified
        if self.start_frame > 0:
            # PyAV seeking is in time units, convert frame to time
            time_base = float(self._stream.time_base)
            frame_duration = 1.0 / self.fps
            seek_time = int(self.start_frame * frame_duration / time_base)
            self._container.seek(seek_time, stream=self._stream)
            self._current_frame = self.start_frame

    async def stop(self) -> None:
        """Close the video file."""
        if self._container:
            self._container.close()
            self._container = None
            self._stream = None

    async def next_frame(self) -> TimestampedFrame:
        """
        Get the next frame from the video file.

        Returns:
            TimestampedFrame with the decoded frame

        Raises:
            EOFError: If end of file is reached and loop is False
        """
        if not self._container or not self._stream:
            raise RuntimeError("Source not started. Call start() first.")

        # Check if we've reached the end frame
        if self.end_frame is not None and self._current_frame >= self.end_frame:
            if self.loop:
                # Seek back to start
                await self.stop()
                await self.start()
            else:
                raise EOFError("End of file reached")

        try:
            # Decode next frame
            for packet in self._container.demux(self._stream):
                for frame in packet.decode():
                    # Convert to RGB numpy array
                    frame_rgb = frame.to_ndarray(format='rgb24')

                    # Create timestamped frame
                    timestamp = float(frame.pts * frame.time_base) if frame.pts else 0.0

                    ts_frame = TimestampedFrame(
                        frame=frame_rgb,
                        timestamp=timestamp,
                        frame_number=self._current_frame,
                        source_id=self.path,
                        metadata={
                            'width': frame.width,
                            'height': frame.height,
                            'format': 'rgb24',
                            'pts': frame.pts,
                        }
                    )

                    self._current_frame += 1
                    return ts_frame

            # If we get here, we've reached EOF
            if self.loop:
                # Seek back to start and try again
                await self.stop()
                await self.start()
                return await self.next_frame()
            else:
                raise EOFError("End of file reached")

        except av.error.EOFError:
            # EOF from PyAV
            if self.loop:
                await self.stop()
                await self.start()
                return await self.next_frame()
            else:
                raise EOFError("End of file reached")
        except Exception as e:
            raise RuntimeError(f"Error decoding frame: {e}")

    def seek(self, frame_number: int) -> None:
        """
        Seek to a specific frame number.

        Args:
            frame_number: Frame number to seek to
        """
        if not self._container or not self._stream:
            raise RuntimeError("Source not started. Call start() first.")

        time_base = float(self._stream.time_base)
        frame_duration = 1.0 / self.fps
        seek_time = int(frame_number * frame_duration / time_base)

        self._container.seek(seek_time, stream=self._stream)
        self._current_frame = frame_number

    def get_metadata(self) -> dict:
        """
        Get video file metadata.

        Returns:
            Dictionary containing video metadata (duration, codec, bitrate, etc.)
        """
        if not self._container or not self._stream:
            raise RuntimeError("Source not started. Call start() first.")

        return {
            'path': self.path,
            'width': self._stream.width,
            'height': self._stream.height,
            'fps': float(self._stream.average_rate) if self._stream.average_rate else self.fps,
            'duration': float(self._stream.duration * self._stream.time_base) if self._stream.duration else None,
            'frames': self._stream.frames,
            'codec': self._stream.codec_context.name,
            'format': self._container.format.name,
            'bit_rate': self._stream.bit_rate,
        }
