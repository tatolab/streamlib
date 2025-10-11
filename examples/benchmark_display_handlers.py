#!/usr/bin/env python3
"""
Benchmark: Old Display (cv2.imshow) vs New GPU Texture Display

Compares performance of:
1. CPU Display (cv2.imshow) - requires GPU→CPU transfer + CPU rendering
2. GPU Texture Display (OpenGL) - direct GPU rendering with async PBO

Expected results:
- CPU Display: ~30 FPS @ 1080p (with 6ms transfer overhead)
- GPU Texture Display: ~36-40 FPS @ 1080p (eliminates transfer, async upload)
"""

import asyncio
import time
from collections import deque

try:
    import torch
    import cv2
except ImportError:
    print("Error: Required packages not installed")
    print("Install with: pip install torch opencv-python")
    exit(1)

from streamlib import StreamRuntime, Stream
from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.clocks import TimedTick
from streamlib.messages import VideoFrame

# Try to import GPU display handler
try:
    from streamlib.handlers import DisplayGPUHandler
    HAS_GPU_DISPLAY = True
except ImportError:
    HAS_GPU_DISPLAY = False
    print("Warning: GPU display handler not available")


class BenchmarkPatternHandler(StreamHandler):
    """Generate animated test pattern on GPU for benchmarking."""

    def __init__(self, width=1920, height=1080):
        super().__init__('pattern')
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu', 'cpu'])
        self.device = None
        self.frame_buffer = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
        else:
            print(f"[{self.handler_id}] No GPU context available")
            return

        # Pre-allocate frame buffer
        self.frame_buffer = torch.empty(
            (self.height, self.width, 3),
            dtype=torch.uint8,
            device=self.device
        )

    async def process(self, tick: TimedTick):
        if not self.device:
            return

        # Animated color pattern
        t = tick.timestamp
        r = int((torch.sin(torch.tensor(t * 0.5)) * 0.5 + 0.5) * 255)
        g = int((torch.sin(torch.tensor(t * 0.7)) * 0.5 + 0.5) * 255)
        b = int((torch.sin(torch.tensor(t * 0.3)) * 0.5 + 0.5) * 255)

        self.frame_buffer[:, :, 0] = r
        self.frame_buffer[:, :, 1] = g
        self.frame_buffer[:, :, 2] = b

        frame = VideoFrame(
            width=self.width,
            height=self.height,
            data=self.frame_buffer,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
        )
        self.outputs['video'].write(frame)


class CPUDisplayHandler(StreamHandler):
    """CPU display using cv2.imshow (baseline for comparison)."""

    def __init__(self, window_name='CPU Display'):
        super().__init__('display-cpu')
        self.window_name = window_name
        self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
        self.frame_times = deque(maxlen=60)
        self.last_frame_time = None
        self.transfer_times = deque(maxlen=100)

    async def on_start(self):
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)
        print(f"[{self.handler_id}] CPU display initialized")

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        # Transfer GPU→CPU
        transfer_start = time.perf_counter()
        if isinstance(frame_msg.data, torch.Tensor):
            if frame_msg.data.is_cuda or frame_msg.data.device.type == 'mps':
                frame_np = frame_msg.data.cpu().numpy()
            else:
                frame_np = frame_msg.data.numpy()
        else:
            frame_np = frame_msg.data
        transfer_time = (time.perf_counter() - transfer_start) * 1000
        self.transfer_times.append(transfer_time)

        # Display
        cv2.imshow(self.window_name, frame_np)
        cv2.waitKey(1)

        # FPS tracking
        current_time = time.perf_counter()
        if self.last_frame_time is not None:
            dt = current_time - self.last_frame_time
            self.frame_times.append(dt)
        self.last_frame_time = current_time

        # Log every 60 frames
        if tick.frame_number % 60 == 0 and len(self.frame_times) > 0:
            avg_fps = 1.0 / (sum(self.frame_times) / len(self.frame_times))
            avg_transfer = sum(self.transfer_times) / len(self.transfer_times)
            print(
                f"[{self.handler_id}] "
                f"FPS: {avg_fps:.1f} | "
                f"Transfer: {avg_transfer:.2f}ms"
            )

    async def on_stop(self):
        cv2.destroyAllWindows()


async def benchmark_cpu_display():
    """Benchmark CPU display handler."""
    print("\n" + "=" * 80)
    print("BENCHMARK 1: CPU Display (cv2.imshow + GPU→CPU transfer)")
    print("=" * 80)

    runtime = StreamRuntime(fps=60, enable_gpu=True)
    pattern = BenchmarkPatternHandler(width=1920, height=1080)
    display = CPUDisplayHandler(window_name='Benchmark: CPU Display')

    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    print("\n[Benchmark] Starting CPU display test (5 seconds)...")
    runtime.start()
    await asyncio.sleep(5)
    await runtime.stop()

    # Calculate averages
    if len(display.frame_times) > 0:
        avg_fps = 1.0 / (sum(display.frame_times) / len(display.frame_times))
        avg_transfer = sum(display.transfer_times) / len(display.transfer_times)

        print("\n" + "=" * 80)
        print("CPU DISPLAY RESULTS:")
        print(f"  Average FPS: {avg_fps:.2f}")
        print(f"  Transfer Time: {avg_transfer:.2f}ms")
        print(f"  Frame Time: {1000/avg_fps:.2f}ms")
        print("=" * 80)

        return {
            'fps': avg_fps,
            'transfer_ms': avg_transfer,
            'frame_ms': 1000 / avg_fps
        }


async def benchmark_gpu_display():
    """Benchmark GPU texture display handler."""
    if not HAS_GPU_DISPLAY:
        print("\nSkipping GPU display benchmark (not available)")
        return None

    print("\n" + "=" * 80)
    print("BENCHMARK 2: GPU Texture Display (OpenGL + async PBO)")
    print("=" * 80)

    runtime = StreamRuntime(fps=60, enable_gpu=True)
    pattern = BenchmarkPatternHandler(width=1920, height=1080)
    display = DisplayGPUHandler(
        name='display-gpu',
        window_name='Benchmark: GPU Texture Display',
        width=1920,
        height=1080
    )

    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    print("\n[Benchmark] Starting GPU texture display test (5 seconds)...")
    runtime.start()
    await asyncio.sleep(5)
    await runtime.stop()

    # Calculate averages
    if len(display.frame_times) > 0:
        avg_fps = 1.0 / (sum(display.frame_times) / len(display.frame_times))
        avg_transfer = sum(display.transfer_times) / len(display.transfer_times)
        avg_upload = sum(display.upload_times) / len(display.upload_times)

        print("\n" + "=" * 80)
        print("GPU TEXTURE DISPLAY RESULTS:")
        print(f"  Average FPS: {avg_fps:.2f}")
        print(f"  Transfer Time: {avg_transfer:.2f}ms")
        print(f"  Upload Time: {avg_upload:.2f}ms")
        print(f"  Frame Time: {1000/avg_fps:.2f}ms")
        print("=" * 80)

        return {
            'fps': avg_fps,
            'transfer_ms': avg_transfer,
            'upload_ms': avg_upload,
            'frame_ms': 1000 / avg_fps
        }


async def main():
    print("=" * 80)
    print("DISPLAY HANDLER BENCHMARK")
    print("=" * 80)
    print("Testing: 1920x1080 @ 60 FPS target")
    print("Hardware: Apple Silicon (MPS)")
    print("=" * 80)

    # Run benchmarks
    cpu_results = await benchmark_cpu_display()
    await asyncio.sleep(2)  # Cool down
    gpu_results = await benchmark_gpu_display()

    # Compare results
    if cpu_results and gpu_results:
        print("\n" + "=" * 80)
        print("COMPARISON")
        print("=" * 80)
        fps_improvement = ((gpu_results['fps'] - cpu_results['fps']) / cpu_results['fps']) * 100
        time_saved = cpu_results['frame_ms'] - gpu_results['frame_ms']

        print(f"CPU Display:         {cpu_results['fps']:.1f} FPS ({cpu_results['frame_ms']:.1f}ms/frame)")
        print(f"GPU Texture Display: {gpu_results['fps']:.1f} FPS ({gpu_results['frame_ms']:.1f}ms/frame)")
        print(f"\nImprovement: +{fps_improvement:.1f}% ({time_saved:.1f}ms saved per frame)")
        print(f"Transfer overhead eliminated: {cpu_results['transfer_ms']:.1f}ms → {gpu_results['transfer_ms']:.1f}ms")
        print("=" * 80)


if __name__ == '__main__':
    asyncio.run(main())
