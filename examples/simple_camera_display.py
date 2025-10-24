"""
Simple camera to display example using decorator API.

This demonstrates the simplest possible camera → display pipeline using
the @camera_processor and @display_processor decorators.

Zero-copy GPU pipeline:
    Camera → WebGPU texture → Display swapchain
"""

from streamlib import camera_processor, display_processor, StreamRuntime


@camera_processor(device_id=None)  # None = first available camera
def camera():
    """Zero-copy camera source - no code needed!"""
    pass


@display_processor(title="Camera Feed - streamlib")
def display():
    """Zero-copy display sink - no code needed!"""
    pass


def main():
    print("🎥 Starting camera-to-display pipeline...")
    print("Press Ctrl+C to stop\n")

    # Create runtime (30 FPS, 1920x1080)
    runtime = StreamRuntime(fps=30, width=1920, height=1080, enable_gpu=True)

    # Add processors to runtime (decorated functions)
    runtime.add_stream(camera)
    runtime.add_stream(display)

    # Connect camera output to display input
    runtime.connect(camera.output_ports().video, display.input_ports().video)

    # Start the pipeline and run until interrupted
    print("✅ Pipeline configured")
    print("✅ Starting runtime...\n")

    # runtime.run() starts and blocks until Ctrl+C
    runtime.run()


if __name__ == "__main__":
    main()
