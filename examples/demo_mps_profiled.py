#!/usr/bin/env python3
"""
MPS Demo with Profiling and Verification

Adds:
- GPU verification with detailed device info
- Per-handler timing
- Metal profiling utilities
- Bottleneck identification
"""

import asyncio
import sys
import numpy as np
import cv2
import time

try:
    import torch
except ImportError:
    print("Error: PyTorch not installed.")
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


# Check MPS and print detailed info
if not torch.backends.mps.is_available():
    print("WARNING: MPS not available!")
    DEVICE = torch.device('cpu')
    CAPABILITY = 'cpu'
else:
    DEVICE = torch.device('mps')
    CAPABILITY = 'gpu'
    print("=" * 80)
    print("GPU VERIFICATION")
    print("=" * 80)
    print(f"✓ MPS Available: True")
    print(f"✓ MPS Built: {torch.backends.mps.is_built()}")
    print(f"✓ Device: {DEVICE}")
    print(f"✓ PyTorch Version: {torch.__version__}")
    print("=" * 80)


class ProfiledPatternHandler(StreamHandler):
    """Pattern generator with profiling."""

    def __init__(self, width=1920, height=1080):
        super().__init__('pattern-profiled')
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=[CAPABILITY])

        # Animation state
        self.ball_x = width // 2
        self.ball_y = height // 2
        self.ball_velocity_x = 400
        self.ball_velocity_y = 300
        self.ball_radius = 40
        self.hue = 0
        self.hue_speed = 120

        # Profiling
        self.process_times = []
        self.device = None
        self.y_grid = None
        self.x_grid = None
        self.color_cache = None  # Pre-allocated color tensor

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            print(f"[{self.handler_id}] Using device: {self.device}")

            # Pre-compute grids
            y_coords = torch.arange(self.height, device=self.device, dtype=torch.float32).view(-1, 1).expand(self.height, self.width)
            x_coords = torch.arange(self.width, device=self.device, dtype=torch.float32).view(1, -1).expand(self.height, self.width)
            self.y_grid = y_coords
            self.x_grid = x_coords

            # Pre-allocate color tensor (reused every frame)
            self.color_cache = torch.empty(3, dtype=torch.uint8, device=self.device)
        else:
            self.device = torch.device('cpu')
            self.color_cache = torch.empty(3, dtype=torch.uint8)
            print(f"[{self.handler_id}] WARNING: No GPU context!")

    def _hsv_to_bgr(self, h_norm, s, v):
        c = v * s
        x = c * (1 - abs((h_norm * 6) % 2 - 1))
        m = v - c
        h_sector = int(h_norm * 6)
        rgb_map = [(c, x, 0), (x, c, 0), (0, c, x), (0, x, c), (x, 0, c), (c, 0, x)]
        r, g, b = rgb_map[min(h_sector, 5)]
        return (int((b + m) * 255), int((g + m) * 255), int((r + m) * 255))

    async def process(self, tick: TimedTick):
        start_time = time.perf_counter()

        dt = tick.delta_time

        # Background
        hue_bg = (tick.timestamp * 30) % 180
        bgr = self._hsv_to_bgr(hue_bg / 180.0, 0.5, 0.8)

        # Allocate from pool
        if self._runtime.gpu_context:
            mem_pool = self._runtime.gpu_context['memory_pool']
            frame = mem_pool.allocate((self.height, self.width, 3), 'uint8')
        else:
            frame = torch.empty((self.height, self.width, 3), dtype=torch.uint8, device=self.device)

        frame[:, :, 0] = bgr[0]
        frame[:, :, 1] = bgr[1]
        frame[:, :, 2] = bgr[2]

        # Ball physics
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

        # Draw circle (use pre-allocated color tensor)
        dist = torch.sqrt((self.x_grid - self.ball_x)**2 + (self.y_grid - self.ball_y)**2)
        mask = dist <= self.ball_radius
        self.color_cache[0] = ball_bgr[0]
        self.color_cache[1] = ball_bgr[1]
        self.color_cache[2] = ball_bgr[2]
        frame[mask] = self.color_cache

        # Write
        video_frame = VideoFrame(
            data=frame,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )
        self.outputs['video'].write(video_frame)

        # Profiling
        elapsed = (time.perf_counter() - start_time) * 1000
        self.process_times.append(elapsed)
        if len(self.process_times) > 60:
            self.process_times.pop(0)


class ProfiledOverlayHandler(StreamHandler):
    """Overlay with profiling."""

    def __init__(self):
        super().__init__('overlay-profiled')
        self.inputs['video'] = VideoInput('video', capabilities=[CAPABILITY])
        self.outputs['video'] = VideoOutput('video', capabilities=[CAPABILITY])
        self.pulse_time = 0.0
        self.device = None
        self.process_times = []
        self.marker_color_cache = None
        self.border_color_cache = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            # Pre-allocate color tensors
            self.marker_color_cache = torch.empty(3, dtype=torch.uint8, device=self.device)
            self.border_color_cache = torch.empty(3, dtype=torch.uint8, device=self.device)
        else:
            self.device = torch.device('cpu')
            self.marker_color_cache = torch.empty(3, dtype=torch.uint8)
            self.border_color_cache = torch.empty(3, dtype=torch.uint8)

    async def process(self, tick: TimedTick):
        start_time = time.perf_counter()

        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data
        h, w = frame.shape[:2]

        self.pulse_time += tick.delta_time
        pulse = int(128 + 127 * math.sin(self.pulse_time * 3.0))

        # Markers and borders (use pre-allocated color tensors)
        marker_size = 30
        self.marker_color_cache[0] = 0
        self.marker_color_cache[1] = pulse
        self.marker_color_cache[2] = 255

        frame[:marker_size, :marker_size] = self.marker_color_cache
        frame[:marker_size, -marker_size:] = self.marker_color_cache
        frame[-marker_size:, :marker_size] = self.marker_color_cache
        frame[-marker_size:, -marker_size:] = self.marker_color_cache

        border_thickness = 5
        self.border_color_cache[0] = pulse
        self.border_color_cache[1] = 255 - pulse
        self.border_color_cache[2] = 128

        frame[:border_thickness, :] = self.border_color_cache
        frame[-border_thickness:, :] = self.border_color_cache
        frame[:, :border_thickness] = self.border_color_cache
        frame[:, -border_thickness:] = self.border_color_cache

        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)

        # Profiling
        elapsed = (time.perf_counter() - start_time) * 1000
        self.process_times.append(elapsed)
        if len(self.process_times) > 60:
            self.process_times.pop(0)


class ProfiledWaveformHandler(StreamHandler):
    """Waveform with profiling."""

    def __init__(self):
        super().__init__('waveform-profiled')
        self.inputs['video'] = VideoInput('video', capabilities=[CAPABILITY])
        self.outputs['video'] = VideoOutput('video', capabilities=[CAPABILITY])
        self.wave_offset = 0.0
        self.device = None
        self.process_times = []
        self.wave_color_cache = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            # Pre-allocate color tensor
            self.wave_color_cache = torch.tensor([0, 255, 255], dtype=torch.uint8, device=self.device)
        else:
            self.device = torch.device('cpu')
            self.wave_color_cache = torch.tensor([0, 255, 255], dtype=torch.uint8)

    async def process(self, tick: TimedTick):
        start_time = time.perf_counter()

        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data
        h, w = frame.shape[:2]

        self.wave_offset += tick.delta_time * 100

        # Compute wave positions
        x_coords = torch.arange(0, w, 4, device=self.device, dtype=torch.float32)
        y_coords = (h // 2 + 50 * torch.sin((x_coords + self.wave_offset) * 0.02)).long()
        y_coords = torch.clamp(y_coords, 2, h - 3)

        # Vectorized thickness: create all (x, y) pairs for thickness at once
        # Instead of Python loop, use broadcasting
        thickness_offsets = torch.arange(-2, 3, device=self.device)  # [-2, -1, 0, 1, 2]

        # Broadcast: (num_points, 1) + (5,) -> (num_points, 5)
        y_all = y_coords.unsqueeze(1) + thickness_offsets
        y_all = torch.clamp(y_all, 0, h - 1)

        # Repeat x coords for each thickness value
        x_all = x_coords.long().unsqueeze(1).expand(-1, 5)

        # Flatten and draw all points at once
        y_flat = y_all.flatten()
        x_flat = x_all.flatten()
        valid = (x_flat >= 0) & (x_flat < w) & (y_flat >= 0) & (y_flat < h)
        frame[y_flat[valid], x_flat[valid]] = self.wave_color_cache

        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)

        # Profiling
        elapsed = (time.perf_counter() - start_time) * 1000
        self.process_times.append(elapsed)
        if len(self.process_times) > 60:
            self.process_times.pop(0)


class ProfiledFPSDisplayHandler(StreamHandler):
    """FPS display with profiling info."""

    def __init__(self, pattern_handler, overlay_handler, waveform_handler):
        super().__init__('fps-display-profiled')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.frame_times = []
        self.fps = 0.0

        # References to other handlers for profiling
        self.pattern_handler = pattern_handler
        self.overlay_handler = overlay_handler
        self.waveform_handler = waveform_handler

        # Self profiling
        self.process_times = []
        self.transfer_times = []
        self.text_times = []

    async def process(self, tick: TimedTick):
        start_time = time.perf_counter()

        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        # Time the GPU→CPU transfer
        transfer_start = time.perf_counter()
        if self._runtime.gpu_context:
            transfer_opt = self._runtime.gpu_context['transfer_optimizer']
            frame_cpu = transfer_opt.to_cpu(frame_msg.data)
        elif isinstance(frame_msg.data, torch.Tensor):
            frame_cpu = frame_msg.data.cpu().numpy()
        else:
            frame_cpu = frame_msg.data
        transfer_time = (time.perf_counter() - transfer_start) * 1000
        self.transfer_times.append(transfer_time)
        if len(self.transfer_times) > 60:
            self.transfer_times.pop(0)

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

        # Time the text rendering
        text_start = time.perf_counter()

        # FPS display
        fps_text = f"FPS: {self.fps:.1f}"
        color = (0, 255, 0) if self.fps >= 55 else ((0, 255, 255) if self.fps >= 40 else (0, 0, 255))

        cv2.putText(frame_cpu, fps_text, (20, 40), cv2.FONT_HERSHEY_SIMPLEX, 1.2, color, 2)
        cv2.putText(frame_cpu, f"dt: {tick.delta_time*1000:.1f}ms", (20, 75), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 255), 2)

        # GPU verification
        gpu_info = f"MPS: {CAPABILITY == 'gpu'} @ {w}x{h}"
        cv2.putText(frame_cpu, gpu_info, (20, 105), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 255, 0) if CAPABILITY == 'gpu' else (255, 0, 0), 2)

        # Profiling info - GPU handlers
        if self.pattern_handler.process_times:
            avg_pattern = sum(self.pattern_handler.process_times) / len(self.pattern_handler.process_times)
            cv2.putText(frame_cpu, f"Pattern: {avg_pattern:.1f}ms", (20, 135), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (255, 255, 255), 1)

        if self.overlay_handler.process_times:
            avg_overlay = sum(self.overlay_handler.process_times) / len(self.overlay_handler.process_times)
            cv2.putText(frame_cpu, f"Overlay: {avg_overlay:.1f}ms", (20, 160), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (255, 255, 255), 1)

        if self.waveform_handler.process_times:
            avg_waveform = sum(self.waveform_handler.process_times) / len(self.waveform_handler.process_times)
            cv2.putText(frame_cpu, f"Waveform: {avg_waveform:.1f}ms", (20, 185), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (255, 255, 255), 1)

        # Profiling info - Transfer and text rendering
        if self.transfer_times:
            avg_transfer = sum(self.transfer_times) / len(self.transfer_times)
            cv2.putText(frame_cpu, f"Transfer: {avg_transfer:.1f}ms", (20, 210), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (255, 255, 0), 1)

        if self.text_times:
            avg_text = sum(self.text_times) / len(self.text_times)
            cv2.putText(frame_cpu, f"Text: {avg_text:.1f}ms", (20, 235), cv2.FONT_HERSHEY_SIMPLEX, 0.5, (255, 255, 0), 1)

        cv2.putText(frame_cpu, f"Frame: {tick.frame_number}", (20, h - 30), cv2.FONT_HERSHEY_SIMPLEX, 0.8, (255, 255, 255), 2)

        text_time = (time.perf_counter() - text_start) * 1000
        self.text_times.append(text_time)
        if len(self.text_times) > 60:
            self.text_times.pop(0)

        video_frame = VideoFrame(
            data=frame_cpu,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)

        # Total profiling
        elapsed = (time.perf_counter() - start_time) * 1000
        self.process_times.append(elapsed)
        if len(self.process_times) > 60:
            self.process_times.pop(0)


class DisplayHandler(StreamHandler):
    """Display handler with profiling."""

    def __init__(self, window_name="MPS Profiled"):
        super().__init__('display')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.window_name = window_name
        self.process_times = []

    async def on_start(self):
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)

    async def process(self, tick: TimedTick):
        start_time = time.perf_counter()

        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data if isinstance(frame_msg.data, np.ndarray) else frame_msg.data
        cv2.imshow(self.window_name, frame)
        cv2.waitKey(1)

        elapsed = (time.perf_counter() - start_time) * 1000
        self.process_times.append(elapsed)
        if len(self.process_times) > 60:
            self.process_times.pop(0)

    async def on_stop(self):
        cv2.destroyAllWindows()


async def main():
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument('--resolution', choices=['hd', '1080p'], default='1080p')
    args = parser.parse_args()

    width = 1920 if args.resolution == '1080p' else 640
    height = 1080 if args.resolution == '1080p' else 480

    print("\n" + "=" * 80)
    print(f"MPS PROFILED DEMO - {width}x{height}")
    print("=" * 80)
    print(f"Target FPS: 60")
    print(f"Resolution: {width}x{height}")
    print("=" * 80)

    # Create handlers
    pattern = ProfiledPatternHandler(width=width, height=height)
    overlay = ProfiledOverlayHandler()
    waveform = ProfiledWaveformHandler()
    fps_display = ProfiledFPSDisplayHandler(pattern, overlay, waveform)
    display = DisplayHandler(f"MPS Profiled - {width}x{height}")

    # Create runtime - FPS = 60!
    runtime = StreamRuntime(fps=60, enable_gpu=True)

    # Add streams
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(overlay, dispatcher='asyncio'))
    runtime.add_stream(Stream(waveform, dispatcher='asyncio'))
    runtime.add_stream(Stream(fps_display, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='threadpool'))

    # Connect
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

    print(f"\n✓ Final FPS: {fps_display.fps:.1f}")


if __name__ == '__main__':
    asyncio.run(main())
