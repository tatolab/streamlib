#!/usr/bin/env python3
"""
Multi-Pipeline Composition Demo

Demonstrates streamlib's Unix-pipe philosophy for video:
  Pipeline 1: Camera → Blur ──┐
                               ├─→ Compositor → Display
  Pipeline 2: Test Pattern ────┘

Each pipeline is independent, then composed together like Unix pipes:
  cat file.txt | grep "error" | ...
  echo "warning" | ...
  ... | compositor | display
"""

import asyncio
import argparse

# GPU handlers required for this demo
from streamlib.handlers import CameraHandlerGPU, TestPatternHandler
from streamlib.handlers.blur_gpu import BlurFilterGPU
from streamlib.handlers.compositor_multi import MultiInputCompositor
from streamlib.handlers.display_gpu import DisplayGPUHandler
from streamlib import StreamRuntime, Stream


def main():
    parser = argparse.ArgumentParser(description='Multi-pipeline composition demo')
    parser.add_argument('--mode', choices=['alpha_blend', 'pip', 'side_by_side', 'vertical_stack', 'grid'],
                        default='pip', help='Compositing mode')
    parser.add_argument('--pip-position', choices=['top_left', 'top_right', 'bottom_left', 'bottom_right'],
                        default='bottom_right', help='PIP position (only for pip mode)')
    parser.add_argument('--pip-scale', type=float, default=0.25, help='PIP scale factor (0.0-1.0)')
    parser.add_argument('--camera', type=str, default='Live Camera', help='Camera device name')
    args = parser.parse_args()

    print("=" * 70)
    print("MULTI-PIPELINE COMPOSITION DEMO")
    print("=" * 70)
    print("Architecture:")
    print("  Pipeline 1: Camera → Blur ──┐")
    print("                               ├─→ Compositor → Display")
    print("  Pipeline 2: Test Pattern ────┘")
    print()
    print(f"Camera: {args.camera}")
    print(f"Compositing mode: {args.mode}")
    if args.mode == 'pip':
        print(f"  PIP position: {args.pip_position}")
        print(f"  PIP scale: {args.pip_scale}")
    print()
    print("This demonstrates streamlib's composability:")
    print("  - Independent pipelines (like Unix processes)")
    print("  - Flexible composition (like Unix pipes)")
    print("  - GPU-accelerated where possible")
    print()
    print("Controls:")
    print("  Press Ctrl+C to quit")
    print("=" * 70)

    async def run_demo():
        # Create runtime
        runtime = StreamRuntime(fps=30)

        # Pipeline 1: Camera → Blur
        camera = CameraHandlerGPU(
            device_name=args.camera,
            width=1920,
            height=1080,
            name='camera'
        )

        blur = BlurFilterGPU(
            kernel_size=15,
            sigma=8.0,
            handler_id='blur'
        )

        # Pipeline 2: Test Pattern (SMPTE color bars)
        test_pattern = TestPatternHandler(
            handler_id='test-pattern',
            width=1920,
            height=1080,
            pattern='smpte_bars'
        )

        # Compositor: Combines both pipelines
        compositor = MultiInputCompositor(
            num_inputs=2,
            mode=args.mode,
            width=1920,
            height=1080,
            pip_position=args.pip_position,
            pip_scale=args.pip_scale,
            name='compositor'
        )

        # Display: Final output
        display = DisplayGPUHandler(
            name='display',
            window_name='Multi-Pipeline Composition',
            width=1920,
            height=1080,
            show_fps=True
        )

        # Add all handlers to runtime
        print("\n✓ Adding handlers:")
        print(f"  camera, blur (Pipeline 1)")
        print(f"  test-pattern (Pipeline 2)")
        print(f"  compositor (combines both)")
        print(f"  display (output)")

        runtime.add_stream(Stream(camera, dispatcher='asyncio'))
        runtime.add_stream(Stream(blur, dispatcher='asyncio'))
        runtime.add_stream(Stream(test_pattern, dispatcher='asyncio'))
        runtime.add_stream(Stream(compositor, dispatcher='asyncio'))
        runtime.add_stream(Stream(display, dispatcher='threadpool'))  # Display blocks

        # Connect Pipeline 1: Camera → Blur → Compositor input_0
        runtime.connect(camera.outputs['video'], blur.inputs['video'])
        runtime.connect(blur.outputs['video'], compositor.inputs['input_0'])

        # Connect Pipeline 2: Test Pattern → Compositor input_1
        runtime.connect(test_pattern.outputs['video'], compositor.inputs['input_1'])

        # Connect Compositor → Display
        runtime.connect(compositor.outputs['video'], display.inputs['video'])

        print("\n" + "=" * 70)
        print("PIPELINE CONNECTIONS:")
        print("  Pipeline 1: camera → blur → compositor.input_0")
        print("  Pipeline 2: test_pattern → compositor.input_1")
        print("  Output: compositor → display")
        print("=" * 70)
        print("\nStarting multi-pipeline composition...\n")
        print("=" * 70)

        # Start runtime
        runtime.start()

        # Run until interrupted
        try:
            while runtime._running:
                await asyncio.sleep(1)
        except KeyboardInterrupt:
            print("\n\nStopping...")

        runtime.stop()

    # Run
    asyncio.run(run_demo())


if __name__ == '__main__':
    main()
