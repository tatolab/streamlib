"""
FileSink - Write video files using PyAV.

This sink encodes and writes frames to video files in common formats (MP4, MOV, AVI, etc.)
using PyAV's FFmpeg bindings.
"""

import av
import numpy as np
from typing import Optional, Literal
from ..base import StreamSink, TimestampedFrame
from ..plugins import register_sink


@register_sink('file')
class FileSink(StreamSink):
    """
    Sink that writes frames to a video file.

    Uses PyAV to encode video files in any format supported by FFmpeg.
    Supports various codecs and quality settings.

    Args:
        path: Path to the output video file
        codec: Video codec to use ('h264', 'h265', 'vp9', 'mpeg4', etc.)
        preset: Encoding preset for quality/speed trade-off
            ('ultrafast', 'fast', 'medium', 'slow', 'veryslow')
        crf: Constant Rate Factor for quality (0-51, lower is better)
            Default: 23 (good quality)
        bitrate: Target bitrate (e.g., '2M', '5M'). If set, overrides CRF.
        pix_fmt: Pixel format for encoding ('yuv420p', 'yuv444p', etc.)
        **kwargs: Additional arguments passed to StreamSink

    Example:
        # H.264 with default quality
        sink = FileSink('output.mp4', codec='h264')
        await sink.start()

        async for frame in source.frames():
            await sink.write_frame(frame)

        await sink.stop()

        # High quality H.265
        sink = FileSink('output.mp4', codec='h265', preset='slow', crf=18)
    """

    def __init__(
        self,
        path: str,
        codec: str = 'h264',
        preset: str = 'medium',
        crf: int = 23,
        bitrate: Optional[str] = None,
        pix_fmt: str = 'yuv420p',
        **kwargs
    ):
        super().__init__(**kwargs)

        self.path = path
        self.codec = codec
        self.preset = preset
        self.crf = crf
        self.bitrate = bitrate
        self.pix_fmt = pix_fmt

        self._container: Optional[av.container.OutputContainer] = None
        self._stream: Optional[av.video.stream.VideoStream] = None
        self._frame_count = 0

    async def start(self) -> None:
        """Open the output file and initialize the encoder."""
        # Create output container
        self._container = av.open(self.path, 'w')

        # Add video stream
        self._stream = self._container.add_stream(self.codec, rate=self.fps)
        self._stream.width = self.width
        self._stream.height = self.height
        self._stream.pix_fmt = self.pix_fmt

        # Set codec options
        if self.bitrate:
            self._stream.bit_rate = self._parse_bitrate(self.bitrate)
        else:
            # Use CRF mode
            self._stream.options = {
                'crf': str(self.crf),
                'preset': self.preset,
            }

        self._frame_count = 0

    async def stop(self) -> None:
        """Close the output file and finalize encoding."""
        if self._container:
            # Flush remaining frames
            if self._stream:
                for packet in self._stream.encode():
                    self._container.mux(packet)

            self._container.close()
            self._container = None
            self._stream = None

    async def write_frame(self, frame: TimestampedFrame) -> None:
        """
        Write a frame to the video file.

        Args:
            frame: The timestamped frame to encode and write
        """
        if not self._container or not self._stream:
            raise RuntimeError("Sink not started. Call start() first.")

        # Convert numpy array to PyAV VideoFrame
        av_frame = av.VideoFrame.from_ndarray(frame.frame, format='rgb24')
        av_frame.pts = self._frame_count

        # Encode and write
        for packet in self._stream.encode(av_frame):
            self._container.mux(packet)

        self._frame_count += 1

    def _parse_bitrate(self, bitrate_str: str) -> int:
        """
        Parse bitrate string (e.g., '2M', '500k') to bits per second.

        Args:
            bitrate_str: Bitrate string

        Returns:
            Bitrate in bits per second
        """
        bitrate_str = bitrate_str.strip().upper()

        if bitrate_str.endswith('K'):
            return int(float(bitrate_str[:-1]) * 1000)
        elif bitrate_str.endswith('M'):
            return int(float(bitrate_str[:-1]) * 1000000)
        elif bitrate_str.endswith('G'):
            return int(float(bitrate_str[:-1]) * 1000000000)
        else:
            return int(bitrate_str)

    def get_stats(self) -> dict:
        """
        Get encoding statistics.

        Returns:
            Dictionary containing encoding stats (frames written, etc.)
        """
        return {
            'path': self.path,
            'codec': self.codec,
            'frames_written': self._frame_count,
            'width': self.width,
            'height': self.height,
            'fps': self.fps,
            'preset': self.preset,
            'crf': self.crf,
            'bitrate': self.bitrate,
        }
