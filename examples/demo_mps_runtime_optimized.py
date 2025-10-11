#!/usr/bin/env python3
"""
MPS Demo Using Runtime GPU Utilities - BEST PRACTICES

Demonstrates how to write efficient GPU handlers by leveraging
runtime-provided GPU infrastructure:
- Memory pooling (reuse tensors)
- Transfer optimization (minimize CPU↔GPU moves)
- Batched operations (reduce kernel launches)

This is the RECOMMENDED way to write GPU handlers.
"""

import asyncio
import sys
import numpy as np
import cv2

try:
    import torch
except ImportError:
    print("Error: PyTorch not installed. Install with: pip install torch torchvision")
    sys.exit(1)

from streamlib import (
    StreamRuntime,
    StreamHandler,
    Stream,
    VideoOutput,
    VideoInput,
)
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick
import math


class RuntimeOptimizedPatternHandler(StreamHandler):
    """
    Pattern generator using runtime GPU utilities.

    Best practices demonstrated:
    - Use runtime.gpu_context for device info
    - Reuse coordinate grids (allocated once)
    - Simple, readable code (runtime handles complexity)
    """

    def __init__(self, width=1920, height=1080):
        super().__init__('pattern-runtime-opt')
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

        # Animation state
        self.ball_x = width // 2
        self.ball_y = height // 2
        self.ball_velocity_x = 400
        self.ball_velocity_y = 300
        self.ball_radius = 40
        self.hue = 0
        self.hue_speed = 120

        # Will be initialized in on_start() when runtime is available
        self.device = None
        self.y_grid = None
        self.x_grid = None

    async def on_start(self):
        """Initialize GPU resources using runtime context."""
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']

            # Pre-compute coordinate grids (allocated ONCE)
            y_coords = torch.arange(self.height, device=self.device, dtype=torch.float32).view(-1, 1).expand(self.height, self.width)
            x_coords = torch.arange(self.width, device=self.device, dtype=torch.float32).view(1, -1).expand(self.height, self.width)
            self.y_grid = y_coords
            self.x_grid = x_coords

            print(f"[{self.handler_id}] Initialized with device: {self.device}")
        else:
            print(f"[{self.handler_id}] Warning: No GPU context, using CPU")
            self.device = torch.device('cpu')

    def _hsv_to_bgr(self, h_norm, s, v):
        """Simple HSV to BGR conversion."""
        c = v * s
        x = c * (1 - abs((h_norm * 6) % 2 - 1))
        m = v - c
        h_sector = int(h_norm * 6)
        rgb_map = [(c, x, 0), (x, c, 0), (0, c, x), (0, x, c), (x, 0, c), (c, 0, x)]
        r, g, b = rgb_map[min(h_sector, 5)]
        return (int((b + m) * 255), int((g + m) * 255), int((r + m) * 255))

    async def process(self, tick: TimedTick):
        dt = tick.delta_time

        # Background color
        hue_bg = (tick.timestamp * 30) % 180
        bgr = self._hsv_to_bgr(hue_bg / 180.0, 0.5, 0.8)

        # Allocate frame from runtime memory pool (or create new)
        if self._runtime.gpu_context:
            mem_pool = self._runtime.gpu_context['memory_pool']
            frame = mem_pool.allocate((self.height, self.width, 3), 'uint8')
        else:
            frame = torch.empty((self.height, self.width, 3), dtype=torch.uint8, device=self.device)

        # Fill background
        frame[:, :, 0] = bgr[0]
        frame[:, :, 1] = bgr[1]
        frame[:, :, 2] = bgr[2]

        # Update ball physics
        self.ball_x += self.ball_velocity_x * dt
        self.ball_y += self.ball_velocity_y * dt

        if self.ball_x - self.ball_radius < 0 or self.ball_x + self.ball_radius > self.width:
            self.ball_velocity_x *= -1
            self.ball_x = max(self.ball_radius, min(self.width - self.ball_radius, self.ball_x))
        if self.ball_y - self.ball_radius < 0 or self.ball_y + self.ball_radius > self.height:
            self.ball_velocity_y *= -1
            self.ball_y = max(self.ball_radius, min(self.height - self.ball_radius, self.ball_y))

        # Ball color
        self.hue = (self.hue + self.hue_speed * dt) % 180
        ball_bgr = self._hsv_to_bgr(self.hue / 180.0, 1.0, 1.0)

        # Draw circle (vectorized on GPU)
        dist = torch.sqrt((self.x_grid - self.ball_x)**2 + (self.y_grid - self.ball_y)**2)
        mask = dist <= self.ball_radius
        color_tensor = torch.tensor(ball_bgr, dtype=torch.uint8, device=self.device)
        frame[mask] = color_tensor

        # Write frame (stays on GPU)
        video_frame = VideoFrame(
            data=frame,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )
        self.outputs['video'].write(video_frame)


class RuntimeOptimizedOverlayHandler(StreamHandler):
    """Simple overlay using runtime utilities."""

    def __init__(self):
        super().__init__('overlay-runtime-opt')
        self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
        self.pulse_time = 0.0
        self.device = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        # Work directly on GPU tensor (no copy)
        frame = frame_msg.data
        h, w = frame.shape[:2]

        self.pulse_time += tick.delta_time
        pulse = int(128 + 127 * math.sin(self.pulse_time * 3.0))

        # Corners and borders (batched GPU operations)
        marker_size = 30
        marker_color = torch.tensor([0, pulse, 255], dtype=torch.uint8, device=self.device)

        frame[:marker_size, :marker_size] = marker_color
        frame[:marker_size, -marker_size:] = marker_color
        frame[-marker_size:, :marker_size] = marker_color
        frame[-marker_size:, -marker_size:] = marker_color

        border_thickness = 5
        border_color = torch.tensor([pulse, 255 - pulse, 128], dtype=torch.uint8, device=self.device)

        frame[:border_thickness, :] = border_color
        frame[-border_thickness:, :] = border_color
        frame[:, :border_thickness] = border_color
        frame[:, -border_thickness:] = border_color

        # Write (still on GPU)
        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class RuntimeOptimizedWaveformHandler(StreamHandler):
    """Waveform overlay using runtime utilities."""

    def __init__(self):
        super().__init__('waveform-runtime-opt')
        self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
        self.wave_offset = 0.0
        self.device = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data
        h, w = frame.shape[:2]

        self.wave_offset += tick.delta_time * 100

        # Compute wave on GPU
        x_coords = torch.arange(0, w, 4, device=self.device, dtype=torch.float32)
        y_coords = (h // 2 + 50 * torch.sin((x_coords + self.wave_offset) * 0.02)).long()
        y_coords = torch.clamp(y_coords, 2, h - 3)

        wave_color = torch.tensor([0, 255, 255], dtype=torch.uint8, device=self.device)

        # Draw wave with thickness
        for t in range(-2, 3):
            y_draw = torch.clamp(y_coords + t, 0, h - 1)
            x_draw = x_coords.long()
            valid = (x_draw >= 0) & (x_draw < w)
            frame[y_draw[valid], x_draw[valid]] = wave_color

        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class FPSDisplayHandler(StreamHandler):
    """FPS display on CPU (text rendering)."""

    def __init__(self):
        super().__init__('fps-display')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])  # Request CPU transfer
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.frame_times = []
        self.fps = 0.0

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        # Use runtime transfer optimizer if available
        if self._runtime.gpu_context:
            transfer_opt = self._runtime.gpu_context['transfer_optimizer']
            frame_cpu = transfer_opt.to_cpu(frame_msg.data)
        elif isinstance(frame_msg.data, torch.Tensor):
            frame_cpu = frame_msg.data.cpu().numpy()
        else:
            frame_cpu = frame_msg.data

        h, w = frame_cpu.shape[:2]

        # Calculate FPS
        self.frame_times.append(tick.timestamp)
        if len(self.frame_times) > 30:
            self.frame_times.pop(0)

        if len(self.frame_times) >= 2:
            time_span = self.frame_times[-1] - self.frame_times[0]
            if time_span > 0.1:
                calculated_fps = (len(self.frame_times) - 1) / time_span
                self.fps = min(calculated_fps, 200.0)

        # Render text
        fps_text = f"FPS: {self.fps:.1f}"
        color = (0, 255, 0) if self.fps >= 55 else ((0, 255, 255) if self.fps >= 40 else (0, 0, 255))

        cv2.putText(frame_cpu, fps_text, (20, 40), cv2.FONT_HERSHEY_SIMPLEX, 1.2, color, 2)
        cv2.putText(frame_cpu, f"dt: {tick.delta_time*1000:.1f}ms", (20, 75), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 255), 2)
        cv2.putText(frame_cpu, f"Runtime GPU: {w}x{h}", (20, 105), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 255), 2)
        cv2.putText(frame_cpu, "Runtime Optimized ✓", (20, 135), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 255, 0), 2)
        cv2.putText(frame_cpu, f"Frame: {tick.frame_number}", (20, h - 30), cv2.FONT_HERSHEY_SIMPLEX, 0.8, (255, 255, 255), 2)

        video_frame = VideoFrame(
            data=frame_cpu,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class DisplayHandler(StreamHandler):
    """Display handler."""

    def __init__(self, window_name="Runtime GPU Demo"):
        super().__init__('display')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.window_name = window_name

    async def on_start(self):
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data if isinstance(frame_msg.data, np.ndarray) else frame_msg.data
        cv2.imshow(self.window_name, frame)
        cv2.waitKey(1)

    async def on_stop(self):
        cv2.destroyAllWindows()


async def main():
    import argparse

    parser = argparse.ArgumentParser(description='Runtime GPU Utilities Demo')
    parser.add_argument('--resolution', choices=['hd', '1080p'], default='1080p',
                       help='Resolution: hd (640x480) or 1080p (1920x1080)')
    args = parser.parse_args()

    width = 1920 if args.resolution == '1080p' else 640
    height = 1080 if args.resolution == '1080p' else 480

    print("="*80)
    print("MPS Demo Using Runtime GPU Utilities - BEST PRACTICES")
    print("="*80)
    print(f"\nResolution: {width}x{height}")
    print("\n🎯 Runtime Features Used:")
    print("  ✓ GPU memory pooling (tensor reuse)")
    print("  ✓ Transfer optimizer (tracks CPU/GPU location)")
    print("  ✓ Auto-detected backend (MPS/CUDA/CPU)")
    print("\n📊 Pipeline:")
    print("  Pattern[GPU] → Overlay[GPU] → Waveform[GPU]")
    print("  → [GPU→CPU auto-transfer] → FPSDisplay[CPU] → Display[CPU]")
    print("\n🚀 Expected Performance:")
    print("  640x480:  ~40-50 FPS (improved from 11 FPS)")
    print("  1920x1080: ~30-40 FPS (better than CPU at this resolution)")
    print("\nPress Ctrl+C to stop...")
    print("="*80)

    # Create handlers
    pattern = RuntimeOptimizedPatternHandler(width=width, height=height)
    overlay = RuntimeOptimizedOverlayHandler()
    waveform = RuntimeOptimizedWaveformHandler()
    fps_display = FPSDisplayHandler()
    display = DisplayHandler(f"🚀 Runtime GPU - {width}x{height}")

    # Create runtime with GPU utilities
    runtime = StreamRuntime(fps=60, enable_gpu=True)

    # Add streams
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(overlay, dispatcher='asyncio'))
    runtime.add_stream(Stream(waveform, dispatcher='asyncio'))
    runtime.add_stream(Stream(fps_display, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='threadpool'))

    # Connect pipeline
    runtime.connect(pattern.outputs['video'], overlay.inputs['video'])
    runtime.connect(overlay.outputs['video'], waveform.inputs['video'])
    runtime.connect(waveform.outputs['video'], fps_display.inputs['video'])
    runtime.connect(fps_display.outputs['video'], display.inputs['video'])

    # Start
    runtime.start()

    try:
        await asyncio.sleep(3600)
    except KeyboardInterrupt:
        print("\n\nStopping...")

    await runtime.stop()

    print(f"\n✅ Demo complete! Final FPS: {fps_display.fps:.1f}")


if __name__ == '__main__':
    asyncio.run(main())
