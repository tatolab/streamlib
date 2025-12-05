# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""
Simple camera to display example using decorator API.

This demonstrates the simplest possible camera → display pipeline using
the @camera_processor and @display_processor decorators.

Zero-copy GPU pipeline:
    Camera → WebGPU texture → Display swapchain
"""

from streamlib import StreamRuntime, CAMERA_PROCESSOR, DISPLAY_PROCESSOR


def main():
    print("🎥 Starting camera-to-display pipeline...")
    print("Press Ctrl+C to stop\n")

    # Create runtime (configuration is per-processor)
    runtime = StreamRuntime()

    # Add processors with explicit keyword arguments and type-safe constants
    camera_handle = runtime.add_processor(
        processor=CAMERA_PROCESSOR,
        config={"device_id": "0x1424001bcf2284"}
    )
    display_handle = runtime.add_processor(
        processor=DISPLAY_PROCESSOR,
        config={"width": 1920, "height": 1080, "title": "Camera Feed - streamlib"}
    )

    # Connect using explicit keyword arguments
    runtime.connect(
        output=camera_handle.output_port("video"),
        input=display_handle.input_port("video")
    )

    # Start the pipeline and run until interrupted
    print("✅ Pipeline configured")
    print("✅ Starting runtime...\n")

    # runtime.run() starts and blocks until Ctrl+C
    runtime.run()


if __name__ == "__main__":
    main()
