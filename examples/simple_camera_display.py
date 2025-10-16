"""
Simple camera to display example using decorator API.

This demonstrates the simplest possible camera → display pipeline using
the @camera_source and @display_sink decorators.

Zero-copy GPU pipeline:
    Camera → WebGPU texture → Display swapchain
"""

import asyncio
from streamlib import camera_source, display_sink, StreamRuntime, Stream


@camera_source(device_id=None)  # None = first available camera
def camera():
    """Zero-copy camera source - no code needed!"""
    pass


@display_sink(title="Camera Feed - streamlib")
def display():
    """Zero-copy display sink - no code needed!"""
    pass


async def main():
    print("🎥 Starting camera-to-display pipeline...")
    print("Press Ctrl+C to stop\n")

    # Create runtime (30 FPS, 1920x1080)
    runtime = StreamRuntime(fps=30, width=1920, height=1080, enable_gpu=True)

    # Add handlers to runtime
    runtime.add_stream(Stream(camera))
    runtime.add_stream(Stream(display))

    # Connect camera output to display input
    runtime.connect(camera.outputs['video'], display.inputs['video'])

    # Start the pipeline and run until interrupted
    print("✅ Pipeline configured")
    print("✅ Starting runtime...\n")

    # runtime.run() starts and blocks until Ctrl+C
    await runtime.run()


if __name__ == "__main__":
    asyncio.run(main())
