"""
Tests for Phase 2: Basic Sources & Sinks

This test verifies that:
1. TestSource can generate test patterns
2. FileSink can write video files
3. FileSource can read video files
4. HLSSink can create HLS streams
5. DisplaySink can initialize (without actually showing window)
"""

import pytest
import numpy as np
import tempfile
import shutil
from pathlib import Path
from streamlib import (
    TestSource,
    FileSource,
    FileSink,
    HLSSink,
    DisplaySink,
    TimestampedFrame,
)


@pytest.mark.asyncio
async def test_test_source_smpte_bars():
    """Test TestSource with SMPTE color bars pattern."""
    source = TestSource(
        pattern='smpte_bars',
        width=640,
        height=480,
        fps=30
    )

    await source.start()

    # Get a frame
    frame = await source.next_frame()

    # Verify frame properties
    assert isinstance(frame, TimestampedFrame)
    assert frame.frame.shape == (480, 640, 3)
    assert frame.frame.dtype == np.uint8
    assert frame.frame_number == 0
    assert frame.source_id == 'test_smpte_bars'

    # Get another frame
    frame2 = await source.next_frame()
    assert frame2.frame_number == 1

    await source.stop()


@pytest.mark.asyncio
async def test_test_source_patterns():
    """Test different test patterns."""
    patterns = ['color_bars', 'solid', 'gradient', 'checkerboard', 'moving_box']

    for pattern in patterns:
        source = TestSource(pattern=pattern, width=320, height=240)
        await source.start()

        frame = await source.next_frame()
        assert frame.frame.shape == (240, 320, 3)
        assert frame.metadata['pattern'] == pattern

        await source.stop()


@pytest.mark.asyncio
async def test_file_sink_write():
    """Test writing video file with FileSink."""
    with tempfile.TemporaryDirectory() as tmpdir:
        output_path = Path(tmpdir) / 'test_output.mp4'

        # Create sink
        sink = FileSink(
            path=str(output_path),
            codec='h264',
            width=320,
            height=240,
            fps=30
        )

        await sink.start()

        # Write some test frames
        for i in range(30):  # 1 second worth of frames
            frame_data = np.random.randint(0, 255, (240, 320, 3), dtype=np.uint8)
            frame = TimestampedFrame(
                frame=frame_data,
                timestamp=i / 30.0,
                frame_number=i
            )
            await sink.write_frame(frame)

        await sink.stop()

        # Verify file was created
        assert output_path.exists()
        assert output_path.stat().st_size > 0

        # Verify stats
        stats = sink.get_stats()
        assert stats['frames_written'] == 30
        assert stats['codec'] == 'h264'


@pytest.mark.asyncio
async def test_file_source_and_sink_roundtrip():
    """Test writing with FileSink and reading back with FileSource."""
    with tempfile.TemporaryDirectory() as tmpdir:
        output_path = Path(tmpdir) / 'roundtrip.mp4'

        # Write video
        sink = FileSink(str(output_path), width=320, height=240, fps=10)
        await sink.start()

        test_frames = []
        for i in range(10):
            # Create distinctive pattern for each frame
            frame_data = np.full((240, 320, 3), i * 25, dtype=np.uint8)
            frame = TimestampedFrame(
                frame=frame_data,
                timestamp=i / 10.0,
                frame_number=i
            )
            test_frames.append(frame_data)
            await sink.write_frame(frame)

        await sink.stop()

        # Read video back
        source = FileSource(str(output_path))
        await source.start()

        # Verify we can read frames
        read_count = 0
        async for frame in source.frames():
            assert frame.frame.shape == (240, 320, 3)
            read_count += 1
            if read_count >= 10:  # Read same number we wrote
                break

        await source.stop()

        # Video encoding may drop/add frames, so check we got at least most of them
        assert read_count >= 9, f"Expected at least 9 frames, got {read_count}"


@pytest.mark.asyncio
async def test_test_source_to_file():
    """Test pipeline: TestSource -> FileSink."""
    with tempfile.TemporaryDirectory() as tmpdir:
        output_path = Path(tmpdir) / 'test_pattern.mp4'

        # Create test source
        source = TestSource(pattern='color_bars', width=640, height=480, fps=30)
        await source.start()

        # Create file sink
        sink = FileSink(str(output_path), width=640, height=480, fps=30)
        await sink.start()

        # Write 30 frames (1 second)
        for i in range(30):
            frame = await source.next_frame()
            await sink.write_frame(frame)

        await source.stop()
        await sink.stop()

        # Verify output
        assert output_path.exists()
        assert output_path.stat().st_size > 0


@pytest.mark.asyncio
async def test_hls_sink():
    """Test HLS streaming output."""
    with tempfile.TemporaryDirectory() as tmpdir:
        output_dir = Path(tmpdir) / 'hls_stream'

        # Create HLS sink
        sink = HLSSink(
            output_dir=str(output_dir),
            segment_duration=2,  # 2 second segments
            max_segments=3,
            width=320,
            height=240,
            fps=30
        )

        await sink.start()

        # Write frames for 3 seconds (should create 2 segments)
        for i in range(90):  # 90 frames at 30fps = 3 seconds
            frame_data = np.random.randint(0, 255, (240, 320, 3), dtype=np.uint8)
            frame = TimestampedFrame(
                frame=frame_data,
                timestamp=i / 30.0,
                frame_number=i
            )
            await sink.write_frame(frame)

        await sink.stop()

        # Verify output
        assert output_dir.exists()

        # Check for playlist
        playlist_path = output_dir / 'playlist.m3u8'
        assert playlist_path.exists()

        # Check for segment files
        segments = list(output_dir.glob('segment_*.ts'))
        assert len(segments) >= 1  # Should have at least 1 segment

        # Verify playlist content
        with open(playlist_path, 'r') as f:
            playlist_content = f.read()
            assert '#EXTM3U' in playlist_content
            assert '#EXT-X-ENDLIST' in playlist_content  # Final playlist


@pytest.mark.asyncio
async def test_display_sink_initialization():
    """Test DisplaySink initialization (without actually showing window)."""
    # Note: This test just verifies initialization, doesn't actually display
    # In CI environments, you may want to skip this test or mock cv2
    sink = DisplaySink(
        window_name='Test Window',
        show_fps=True,
        width=320,
        height=240
    )

    # Just verify the sink can be created with correct properties
    assert sink.window_name == 'Test Window'
    assert sink.show_fps == True
    assert sink.width == 320
    assert sink.height == 240


@pytest.mark.asyncio
async def test_file_source_metadata():
    """Test FileSource metadata extraction."""
    with tempfile.TemporaryDirectory() as tmpdir:
        output_path = Path(tmpdir) / 'metadata_test.mp4'

        # Create a test video
        sink = FileSink(str(output_path), width=640, height=480, fps=30, codec='h264')
        await sink.start()

        for i in range(60):  # 2 seconds
            frame_data = np.zeros((480, 640, 3), dtype=np.uint8)
            frame = TimestampedFrame(frame=frame_data, timestamp=i/30.0, frame_number=i)
            await sink.write_frame(frame)

        await sink.stop()

        # Read and check metadata
        source = FileSource(str(output_path))
        await source.start()

        metadata = source.get_metadata()
        assert metadata['width'] == 640
        assert metadata['height'] == 480
        assert metadata['codec'] == 'h264'
        assert metadata['format'] == 'mov,mp4,m4a,3gp,3g2,mj2'

        await source.stop()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
