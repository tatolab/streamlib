"""
HLSSink - HTTP Live Streaming output.

This sink generates HLS streams with segments and playlists for web streaming.
"""

import av
import os
import time
from pathlib import Path
from typing import Optional, List
from ..base import StreamSink, TimestampedFrame
from ..plugins import register_sink


@register_sink('hls')
class HLSSink(StreamSink):
    """
    Sink that generates HTTP Live Streaming (HLS) output.

    Creates video segments and m3u8 playlist files for web streaming.
    Suitable for serving video over HTTP to web browsers.

    Args:
        output_dir: Directory to write HLS segments and playlist
        segment_duration: Duration of each segment in seconds (default: 6)
        max_segments: Maximum number of segments to keep (default: 5)
            Older segments are deleted to manage disk space
        codec: Video codec to use ('h264', 'h265')
        preset: Encoding preset ('ultrafast', 'fast', 'medium', 'slow')
        crf: Constant Rate Factor for quality (0-51, lower is better)
        **kwargs: Additional arguments passed to StreamSink

    Example:
        # Create HLS stream in 'stream' directory
        sink = HLSSink(output_dir='stream', segment_duration=6)
        await sink.start()

        async for frame in source.frames():
            await sink.write_frame(frame)

        await sink.stop()

        # Serve the stream directory over HTTP
        # Files: stream/playlist.m3u8, stream/segment_0.ts, segment_1.ts, ...
    """

    def __init__(
        self,
        output_dir: str,
        segment_duration: int = 6,
        max_segments: int = 5,
        codec: str = 'h264',
        preset: str = 'fast',
        crf: int = 23,
        **kwargs
    ):
        super().__init__(**kwargs)

        self.output_dir = Path(output_dir)
        self.segment_duration = segment_duration
        self.max_segments = max_segments
        self.codec = codec
        self.preset = preset
        self.crf = crf

        self._segment_index = 0
        self._current_segment: Optional[av.container.OutputContainer] = None
        self._current_stream: Optional[av.video.stream.VideoStream] = None
        self._segment_frame_count = 0
        self._total_frame_count = 0
        self._segment_files: List[str] = []
        self._playlist_path = self.output_dir / 'playlist.m3u8'

    async def start(self) -> None:
        """Create output directory and initialize HLS streaming."""
        # Create output directory
        self.output_dir.mkdir(parents=True, exist_ok=True)

        # Start first segment
        await self._start_new_segment()

    async def stop(self) -> None:
        """Close current segment and finalize playlist."""
        if self._current_segment:
            await self._close_current_segment()

        # Write final playlist
        self._write_playlist(is_final=True)

    async def write_frame(self, frame: TimestampedFrame) -> None:
        """
        Write a frame to the HLS stream.

        Automatically creates new segments based on segment_duration.

        Args:
            frame: The timestamped frame to encode and write
        """
        if not self._current_segment or not self._current_stream:
            raise RuntimeError("Sink not started. Call start() first.")

        # Check if we need to start a new segment
        segment_time = self._segment_frame_count / self.fps
        if segment_time >= self.segment_duration:
            await self._close_current_segment()
            await self._start_new_segment()

        # Convert numpy array to PyAV VideoFrame
        av_frame = av.VideoFrame.from_ndarray(frame.frame, format='rgb24')
        av_frame.pts = self._segment_frame_count

        # Encode and write
        for packet in self._current_stream.encode(av_frame):
            self._current_segment.mux(packet)

        self._segment_frame_count += 1
        self._total_frame_count += 1

    async def _start_new_segment(self) -> None:
        """Start a new HLS segment."""
        # Generate segment filename
        segment_filename = f'segment_{self._segment_index}.ts'
        segment_path = self.output_dir / segment_filename

        # Create output container for segment
        self._current_segment = av.open(str(segment_path), 'w', format='mpegts')

        # Add video stream
        self._current_stream = self._current_segment.add_stream(
            self.codec,
            rate=self.fps
        )
        self._current_stream.width = self.width
        self._current_stream.height = self.height
        self._current_stream.pix_fmt = 'yuv420p'

        # Set codec options
        self._current_stream.options = {
            'crf': str(self.crf),
            'preset': self.preset,
        }

        self._segment_frame_count = 0
        self._segment_files.append(segment_filename)
        self._segment_index += 1

        # Remove old segments if we exceed max_segments
        if len(self._segment_files) > self.max_segments:
            old_segment = self._segment_files.pop(0)
            old_path = self.output_dir / old_segment
            if old_path.exists():
                old_path.unlink()

    async def _close_current_segment(self) -> None:
        """Close the current HLS segment."""
        if self._current_segment and self._current_stream:
            # Flush remaining frames
            for packet in self._current_stream.encode():
                self._current_segment.mux(packet)

            self._current_segment.close()
            self._current_segment = None
            self._current_stream = None

        # Update playlist
        self._write_playlist(is_final=False)

    def _write_playlist(self, is_final: bool = False) -> None:
        """
        Write HLS playlist (m3u8 file).

        Args:
            is_final: Whether this is the final playlist (end of stream)
        """
        with open(self._playlist_path, 'w') as f:
            f.write('#EXTM3U\n')
            f.write('#EXT-X-VERSION:3\n')
            f.write(f'#EXT-X-TARGETDURATION:{self.segment_duration}\n')
            f.write(f'#EXT-X-MEDIA-SEQUENCE:{max(0, self._segment_index - len(self._segment_files))}\n')

            # Write segment entries
            for segment_file in self._segment_files:
                f.write(f'#EXTINF:{self.segment_duration:.3f},\n')
                f.write(f'{segment_file}\n')

            # End of stream marker
            if is_final:
                f.write('#EXT-X-ENDLIST\n')

    def get_playlist_url(self) -> str:
        """
        Get the playlist URL (relative path).

        Returns:
            Path to playlist.m3u8 file
        """
        return str(self._playlist_path)

    def get_stats(self) -> dict:
        """
        Get HLS streaming statistics.

        Returns:
            Dictionary containing streaming stats
        """
        return {
            'output_dir': str(self.output_dir),
            'total_frames': self._total_frame_count,
            'segment_index': self._segment_index,
            'active_segments': len(self._segment_files),
            'segment_duration': self.segment_duration,
            'max_segments': self.max_segments,
            'codec': self.codec,
        }
