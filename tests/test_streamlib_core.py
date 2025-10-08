"""
Tests for streamlib core infrastructure.

This test verifies that:
1. Basic imports work
2. DrawingLayer can be created and render frames
3. DefaultCompositor can composite layers
4. Plugin registration works
"""

import pytest
import numpy as np
from streamlib import (
    TimestampedFrame,
    DrawingLayer,
    VideoLayer,
    DefaultCompositor,
    get_registry,
)


def test_imports():
    """Test that all core imports work."""
    from streamlib import (
        StreamSource,
        StreamSink,
        Layer,
        Compositor,
        TimestampedFrame,
        FrameTimer,
        PTPClient,
        MultiStreamSynchronizer,
        DrawingLayer,
        VideoLayer,
        DefaultCompositor,
    )
    assert True


def test_timestamped_frame():
    """Test TimestampedFrame creation."""
    frame = np.zeros((480, 640, 3), dtype=np.uint8)
    ts_frame = TimestampedFrame(
        frame=frame,
        timestamp=1.0,
        frame_number=42,
        ptp_time=1.000001,
        source_id="test_source"
    )

    assert ts_frame.frame.shape == (480, 640, 3)
    assert ts_frame.timestamp == 1.0
    assert ts_frame.frame_number == 42
    assert ts_frame.ptp_time == 1.000001
    assert ts_frame.source_id == "test_source"


@pytest.mark.asyncio
async def test_drawing_layer_basic():
    """Test basic DrawingLayer functionality."""
    draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))
    canvas.drawCircle(ctx.width / 2, ctx.height / 2, 50, paint)
"""

    layer = DrawingLayer(
        name="test_circle",
        draw_code=draw_code,
        width=640,
        height=480
    )

    # Process a frame
    result = await layer.process_frame(None, 640, 480)

    # Verify output
    assert result.shape == (480, 640, 4)  # RGBA
    assert result.dtype == np.uint8

    # Check that something was drawn (center should be red)
    center_y, center_x = 240, 320
    center_pixel = result[center_y, center_x]
    assert center_pixel[0] > 200  # Red channel should be high
    assert center_pixel[3] > 0    # Alpha should be non-zero


@pytest.mark.asyncio
async def test_drawing_layer_with_context():
    """Test DrawingLayer with custom context variables."""
    draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 255, 255, 255))
    font = skia.Font(None, 24)
    canvas.drawString(ctx.message, 10, 30, font, paint)
"""

    layer = DrawingLayer(
        name="test_text",
        draw_code=draw_code,
        width=640,
        height=480
    )

    # Update context with custom variable
    layer.update_context(message="Hello, World!")

    # Process a frame
    result = await layer.process_frame(None, 640, 480)

    # Verify output
    assert result.shape == (480, 640, 4)
    assert result.dtype == np.uint8


@pytest.mark.asyncio
async def test_video_layer():
    """Test VideoLayer pass-through."""
    # Create a test frame (RGB)
    input_frame_data = np.random.randint(0, 255, (480, 640, 3), dtype=np.uint8)
    input_frame = TimestampedFrame(
        frame=input_frame_data,
        timestamp=1.0,
        frame_number=1
    )

    layer = VideoLayer(name="video")

    # Process the frame
    result = await layer.process_frame(input_frame, 640, 480)

    # Verify output is RGBA
    assert result.shape == (480, 640, 4)
    assert result.dtype == np.uint8

    # Verify RGB channels match input
    np.testing.assert_array_equal(result[:, :, :3], input_frame_data)

    # Verify alpha is fully opaque
    assert np.all(result[:, :, 3] == 255)


@pytest.mark.asyncio
async def test_compositor_basic():
    """Test basic compositor functionality."""
    compositor = DefaultCompositor(width=640, height=480)

    # Add a drawing layer
    draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))
    canvas.drawRect(skia.Rect(100, 100, 200, 200), paint)
"""

    layer = DrawingLayer(
        name="red_box",
        draw_code=draw_code,
        width=640,
        height=480,
        z_index=1
    )

    compositor.add_layer(layer)

    # Composite a frame
    result = await compositor.composite()

    # Verify output
    assert isinstance(result, TimestampedFrame)
    assert result.frame.shape == (480, 640, 3)  # RGB output
    assert result.frame.dtype == np.uint8

    # Verify the red box was drawn
    box_pixel = result.frame[150, 150]  # Inside the box
    assert box_pixel[0] > 200  # Red channel


@pytest.mark.asyncio
async def test_compositor_layer_ordering():
    """Test that layers are composited in z_index order."""
    compositor = DefaultCompositor(width=640, height=480)

    # Add two overlapping layers with different z-indices
    layer1_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))  # Red
    canvas.drawRect(skia.Rect(100, 100, 300, 300), paint)
"""

    layer2_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(0, 0, 255, 255))  # Blue
    canvas.drawRect(skia.Rect(200, 200, 400, 400), paint)
"""

    # Add layer1 with z_index=1 (bottom)
    layer1 = DrawingLayer(
        name="red_layer",
        draw_code=layer1_code,
        width=640,
        height=480,
        z_index=1
    )

    # Add layer2 with z_index=2 (top)
    layer2 = DrawingLayer(
        name="blue_layer",
        draw_code=layer2_code,
        width=640,
        height=480,
        z_index=2
    )

    compositor.add_layer(layer1)
    compositor.add_layer(layer2)

    # Composite
    result = await compositor.composite()

    # In the overlap area (250, 250), blue should be on top
    overlap_pixel = result.frame[250, 250]
    assert overlap_pixel[2] > 200  # Blue channel should be high


@pytest.mark.asyncio
async def test_compositor_layer_visibility():
    """Test layer visibility control."""
    compositor = DefaultCompositor(width=640, height=480)

    draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))
    canvas.drawCircle(320, 240, 100, paint)
"""

    layer = DrawingLayer(
        name="circle",
        draw_code=draw_code,
        width=640,
        height=480,
        visible=True
    )

    compositor.add_layer(layer)

    # Composite with layer visible
    result1 = await compositor.composite()
    center_pixel_1 = result1.frame[240, 320]

    # Hide layer
    layer.set_visible(False)

    # Composite with layer hidden
    result2 = await compositor.composite()
    center_pixel_2 = result2.frame[240, 320]

    # The center pixel should be different (layer hidden vs visible)
    assert not np.array_equal(center_pixel_1, center_pixel_2)


def test_plugin_registry():
    """Test plugin registration system."""
    registry = get_registry()

    # Check that our built-in plugins are registered
    assert 'drawing' in registry.list_layers()
    assert 'default' in registry.list_compositors()

    # Verify we can get the classes
    drawing_layer_class = registry.get_layer('drawing')
    assert drawing_layer_class is DrawingLayer

    compositor_class = registry.get_compositor('default')
    assert compositor_class is DefaultCompositor


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
