#!/usr/bin/env python3
"""
Animated Performance Demo - Showcases Fixed Clock Broadcast

Features:
- Animated bouncing ball with color cycling
- Color cycling background
- Pulsing corner markers (animated overlays)
- Animated waveform visualization
- Real-time FPS counter showing actual performance
- Delta time display for frame timing
- Smooth 60 FPS playback with frame-rate independent movement

This demo proves the clock tick broadcast fix is working.
All handlers receive the same tick concurrently, achieving smooth frame-rate
independent animation using delta time (just like game engines).

Pipeline: 5 concurrent handlers processing 640x480 frames @ 60 FPS target
"""

import asyncio
import sys
import numpy as np
import cv2
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


class AnimatedPatternHandler(StreamHandler):
    """Generate animated patterns with bouncing ball."""

    def __init__(self, width=640, height=480):  # Reduced from 1280x720
        super().__init__('animated-pattern')
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

        # Animation state (using velocity in pixels/second for frame-rate independence)
        self.ball_x = width // 2
        self.ball_y = height // 2
        self.ball_velocity_x = 400  # pixels per second
        self.ball_velocity_y = 300  # pixels per second
        self.ball_radius = 40
        self.hue = 0
        self.hue_speed = 120  # degrees per second

    async def process(self, tick: TimedTick):
        # Use delta_time for frame-rate independent animation (just like games!)
        dt = tick.delta_time

        # Create frame with simple color cycling background
        hue_bg = (tick.timestamp * 30) % 180  # Rotate through hues
        color_hsv = np.uint8([[[int(hue_bg), 50, 80]]])
        color_bgr = cv2.cvtColor(color_hsv, cv2.COLOR_HSV2BGR)[0][0]
        frame = np.full((self.height, self.width, 3), color_bgr, dtype=np.uint8)

        # Update ball position using delta_time (frame-rate independent physics!)
        self.ball_x += self.ball_velocity_x * dt
        self.ball_y += self.ball_velocity_y * dt

        # Bounce off walls
        if self.ball_x - self.ball_radius < 0 or self.ball_x + self.ball_radius > self.width:
            self.ball_velocity_x *= -1
            self.ball_x = max(self.ball_radius, min(self.width - self.ball_radius, self.ball_x))
        if self.ball_y - self.ball_radius < 0 or self.ball_y + self.ball_radius > self.height:
            self.ball_velocity_y *= -1
            self.ball_y = max(self.ball_radius, min(self.height - self.ball_radius, self.ball_y))

        # Color cycling using delta_time
        self.hue = (self.hue + self.hue_speed * dt) % 180
        color_hsv = np.uint8([[[int(self.hue), 255, 255]]])
        color_bgr = cv2.cvtColor(color_hsv, cv2.COLOR_HSV2BGR)[0][0]

        # Draw bouncing ball
        cv2.circle(frame, (int(self.ball_x), int(self.ball_y)),
                   self.ball_radius, color_bgr.tolist(), -1)

        # Frame info overlay
        info_text = f"Frame: {tick.frame_number}"
        cv2.putText(frame, info_text, (20, self.height - 30),
                   cv2.FONT_HERSHEY_SIMPLEX, 0.8, (255, 255, 255), 2)

        # Write frame
        video_frame = VideoFrame(
            data=frame,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )
        self.outputs['video'].write(video_frame)


class PulsingOverlayHandler(StreamHandler):
    """Add pulsing corner markers and frame border."""

    def __init__(self):
        super().__init__('pulsing-overlay')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.pulse_time = 0.0

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data.copy()
        h, w = frame.shape[:2]

        # Pulsing intensity using delta_time (frame-rate independent!)
        self.pulse_time += tick.delta_time
        pulse = int(128 + 127 * math.sin(self.pulse_time * 3.0))

        # Corner markers (pulsing)
        marker_size = 30
        corners = [(0, 0), (w - marker_size, 0), (0, h - marker_size), (w - marker_size, h - marker_size)]
        for x, y in corners:
            cv2.rectangle(frame, (x, y), (x + marker_size, y + marker_size),
                         (0, pulse, 255), -1)

        # Animated border
        border_thickness = 5
        border_color = (pulse, 255 - pulse, 128)
        cv2.rectangle(frame, (0, 0), (w-1, h-1), border_color, border_thickness)

        # Write frame
        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class WaveformOverlayHandler(StreamHandler):
    """Add animated waveform visualization."""

    def __init__(self):
        super().__init__('waveform-overlay')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.wave_offset = 0.0

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data.copy()
        h, w = frame.shape[:2]

        # Animate wave using delta_time (frame-rate independent!)
        self.wave_offset += tick.delta_time * 100  # pixels per second

        # Draw sine wave
        points = []
        for x in range(0, w, 4):
            y = int(h // 2 + 50 * math.sin((x + self.wave_offset) * 0.02))
            points.append((x, y))

        for i in range(len(points) - 1):
            cv2.line(frame, points[i], points[i+1], (0, 255, 255), 3)

        # Write frame
        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class FPSDisplayHandler(StreamHandler):
    """Display FPS counter and performance info."""

    def __init__(self):
        super().__init__('fps-display')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.frame_times = []
        self.fps = 0.0

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data.copy()
        h, w = frame.shape[:2]

        # Calculate FPS with overflow protection
        self.frame_times.append(tick.timestamp)
        if len(self.frame_times) > 30:
            self.frame_times.pop(0)

        if len(self.frame_times) >= 2:
            time_span = self.frame_times[-1] - self.frame_times[0]
            # Protect against very small time_span causing overflow
            if time_span > 0.1:  # At least 100ms span needed
                calculated_fps = (len(self.frame_times) - 1) / time_span
                # Cap FPS display at reasonable maximum (200 FPS)
                self.fps = min(calculated_fps, 200.0)

        # FPS overlay
        fps_text = f"FPS: {self.fps:.1f}"
        font = cv2.FONT_HERSHEY_SIMPLEX
        font_scale = 1.2
        thickness = 2

        # FPS text color based on performance (60 FPS target)
        if self.fps >= 55:
            color = (0, 255, 0)  # Green: Good!
        elif self.fps >= 40:
            color = (0, 255, 255)  # Yellow: OK
        else:
            color = (0, 0, 255)  # Red: Slow

        cv2.putText(frame, fps_text, (20, 40), font, font_scale, color, thickness)

        # Delta time info (for debugging frame timing)
        dt_text = f"dt: {tick.delta_time*1000:.1f}ms"
        cv2.putText(frame, dt_text, (20, 75), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 255), 2)

        # Performance info
        info_text = f"Pipeline: 5 Handlers"
        cv2.putText(frame, info_text, (20, 105), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (255, 255, 255), 2)

        status_text = "Delta Time: Enabled ✓"
        cv2.putText(frame, status_text, (20, 135), cv2.FONT_HERSHEY_SIMPLEX, 0.6, (0, 255, 0), 2)

        # Write frame
        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h
        )
        self.outputs['video'].write(video_frame)


class DisplayHandler(StreamHandler):
    """Display final composited frame."""

    def __init__(self, window_name="Performance Demo"):
        super().__init__('display')
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.window_name = window_name

    async def on_start(self):
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)

    async def process(self, tick: TimedTick):
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        # Display
        cv2.imshow(self.window_name, frame_msg.data)
        cv2.waitKey(1)

    async def on_stop(self):
        cv2.destroyAllWindows()


async def main():
    print("="*80)
    print("Animated Performance Demo")
    print("="*80)
    print("\nShowcasing the fixed clock tick broadcast!")
    print("\n🎯 What you'll see:")
    print("  • Bouncing ball with color cycling")
    print("  • Color cycling background")
    print("  • Pulsing corner markers")
    print("  • Animated waveform visualization")
    print("  • Real-time FPS counter (should show ~55-60 FPS!)")
    print("  • Delta time display (~16.7ms per frame)")
    print("\n📊 Pipeline:")
    print("  AnimatedPattern → PulsingOverlay → Waveform → FPSDisplay → Display")
    print("  (5 handlers, 640x480 @ 60 FPS target)")
    print("\n🚀 Performance:")
    print("  Before fix: ~5 FPS (sequential ticks)")
    print("  After fix: Smooth 60 FPS (broadcast ticks + delta time)")
    print("\nPress Ctrl+C to stop...")
    print("="*80)

    # Create handlers (full pipeline with all effects!)
    pattern = AnimatedPatternHandler(width=640, height=480)
    overlay = PulsingOverlayHandler()
    waveform = WaveformOverlayHandler()
    fps_display = FPSDisplayHandler()
    display = DisplayHandler("🚀 Performance Demo - Fixed Clock Broadcast!")

    # Create runtime (60 FPS target)
    runtime = StreamRuntime(fps=60)

    # Add streams (all concurrent!)
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(overlay, dispatcher='asyncio'))
    runtime.add_stream(Stream(waveform, dispatcher='asyncio'))
    runtime.add_stream(Stream(fps_display, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='threadpool'))

    # Connect full pipeline
    runtime.connect(pattern.outputs['video'], overlay.inputs['video'])
    runtime.connect(overlay.outputs['video'], waveform.inputs['video'])
    runtime.connect(waveform.outputs['video'], fps_display.inputs['video'])
    runtime.connect(fps_display.outputs['video'], display.inputs['video'])

    # Start
    runtime.start()

    # Run until interrupted
    try:
        await asyncio.sleep(3600)  # 1 hour
    except KeyboardInterrupt:
        print("\n\nStopping...")

    await runtime.stop()

    print("\n✅ Demo complete!")
    print(f"Final FPS: {fps_display.fps:.1f}")


if __name__ == '__main__':
    asyncio.run(main())
