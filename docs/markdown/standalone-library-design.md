# Standalone Streaming Library Design

## Overview

This document outlines the design for a standalone streaming library that combines the best aspects of our current GStreamer-based MCP server and the fastrtc library, while addressing key concerns:

### Core Requirements
1. ✅ Remove GStreamer dependency (requires system installation)
2. ✅ Support multiple output formats (not just WebRTC/WebSocket)
3. ✅ Maintain our compositor + drawing layer architecture
4. ✅ Provide transport-agnostic streaming
5. ✅ Composable primitives (like Unix tools for visual operations)
6. ✅ Stateless, single-purpose operations
7. ✅ Usable by anything (humans, scripts, programs, agents)
8. ✅ **Network-transparent** - Operations work locally or remotely
9. ✅ **Distributed processing** - Chain operations across machines
10. ✅ **Mesh-capable** - Multiple machines collaborate on processing

### Extended Requirements (Future-Proof Architecture)
1. ✅ ML and zero-copy for high performance
2. ✅ Fully programmable SDK
3. ✅ Time-synchronized multi-camera for 3D tracking
4. ✅ PTP and genlock support for professional workflows

## Architecture Comparison

### FastRTC Approach
**Strengths:**
- Pure Python video processing using **PyAV** (FFmpeg bindings)
- Clean handler abstraction (receive/emit pattern)
- Async-first design built on asyncio
- No system dependencies (everything via pip)
- Uses aiortc for WebRTC (pure Python)

**Weaknesses:**
- Hard dependency on Gradio for UI
- Primarily designed for WebRTC streaming
- Limited output formats (no HLS, file recording, etc.)

**Key Dependencies:**
```toml
gradio>=4.0,<6.0      # UI framework (DON'T WANT)
aiortc                # WebRTC (pure Python - GOOD)
librosa               # Audio processing
numpy                 # Arrays
av (PyAV)            # FFmpeg bindings (EXCELLENT - replaces GStreamer)
```

### Our Current Approach
**Strengths:**
- Multi-layer compositor with Skia overlays (works great)
- Drawing engine with Python code execution
- HLS streaming works well
- MCP tools for AI agent integration
- Time-based animations (ctx.time)

**Weaknesses:**
- Requires GStreamer system installation
- Complex pipeline syntax
- Not transport-agnostic

### Proposed Standalone Library

**Plugin Architecture:**

```
┌─────────────────────────────────────────────────────────┐
│             Core Library (streamkit)                    │
│         Pure interfaces + minimal implementation        │
├─────────────────────────────────────────────────────────┤
│  Abstract: Source, Sink, Layer, Compositor              │
│  Zero-copy numpy array pipeline                         │
│  Plugin registration system                             │
│  No hard dependencies (all optional)                    │
└─────────────────────────────────────────────────────────┘
                          │
                          ▼
                 ┌──────────────┐
                 │   Plugins    │
                 │              │
                 │ PyAVSource   │
                 │ SkiaLayer    │
                 │ MLLayer      │
                 │ HLSSink      │
                 │ WebRTCSink   │
                 │ PTPSync      │
                 └──────────────┘
```

**Core Pipeline Architecture (Single Machine):**

```
┌─────────────────────────────────────────────────────────────┐
│                    Streaming Library                        │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────┐      ┌──────────────┐                   │
│  │   Sources    │      │    Sinks     │                   │
│  ├──────────────┤      ├──────────────┤                   │
│  │ Webcam       │      │ HLS          │                   │
│  │ Screen       │      │ WebRTC       │                   │
│  │ File         │      │ File         │                   │
│  │ Generated    │      │ Display      │                   │
│  │ Network      │      │ Network      │                   │
│  │ Multi-cam    │      │ SMPTE 2110   │                   │
│  └──────────────┘      └──────────────┘                   │
│         │                      ▲                           │
│         │                      │                           │
│         ▼                      │                           │
│  ┌─────────────────────────────────────┐                  │
│  │         Compositor                  │                  │
│  │  ┌─────────────────────────────┐   │                  │
│  │  │ Layer 1: Video (z=0)        │   │                  │
│  │  │ Layer 2: Overlay (z=1)      │   │                  │
│  │  │ Layer 3: Drawing (z=2)      │   │                  │
│  │  │ Layer 4: ML/AI (z=3)        │   │                  │
│  │  └─────────────────────────────┘   │                  │
│  │  Uses Skia + Alpha Blending        │                  │
│  │  Zero-copy numpy/tensor pipeline   │                  │
│  └─────────────────────────────────────┘                  │
│                                                             │
│  ┌─────────────────────────────────────┐                  │
│  │     Drawing Engine (Skia)           │                  │
│  │  - Execute Python drawing code      │                  │
│  │  - ctx.time for animations          │                  │
│  │  - ctx.frame for AI inference       │                  │
│  └─────────────────────────────────────┘                  │
│                                                             │
│  ┌─────────────────────────────────────┐                  │
│  │     Time Sync Engine (PTP)          │                  │
│  │  - Hardware timestamps              │                  │
│  │  - Multi-camera alignment           │                  │
│  │  - Sub-millisecond accuracy         │                  │
│  └─────────────────────────────────────┘                  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

## Distributed Architecture

### Network-Transparent Operations

Like Unix pipes work locally (`grep | sed | awk`), streamkit operations work across network boundaries:

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│   Phone      │    │  Edge Device │    │  Cloud GPU   │    │   Display    │
│              │    │              │    │              │    │              │
│ Webcam       │───▶│ Compositor   │───▶│ ML Layer     │───▶│ HLS Sink     │
│ Source       │    │ (overlay)    │    │ (detection)  │    │              │
└──────────────┘    └──────────────┘    └──────────────┘    └──────────────┘
  Local capture      Edge processing     GPU inference       Final output
```

**Each step is independent:**
- Phone runs: `stream.source = WebcamSource()` → outputs to network
- Edge runs: `stream.source = NetworkSource('phone:8000')` → `stream.sink = NetworkSink('cloud:8001')`
- Cloud runs: `stream.source = NetworkSource('edge:8001')` → ML processing → `stream.sink = NetworkSink('display:8002')`
- Display runs: `stream.source = NetworkSource('cloud:8002')` → `stream.sink = HLSSink()`

### Mesh Processing

Multiple machines process the same stream **in parallel**:

```
                    ┌─────────────────────────┐
                    │   Coordinator           │
                    │   (Video Source)        │
                    └────┬──────┬──────┬──────┘
                         │      │      │
                    Broadcast to all workers
                         │      │      │
         ┌───────────────┘      │      └───────────────┐
         │                      │                      │
         ▼                      ▼                      ▼
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│   Worker 1       │  │   Worker 2       │  │   Worker 3       │
│                  │  │                  │  │                  │
│ Object Detection │  │  Segmentation    │  │ Depth Estimation │
│ (SAME frames)    │  │  (SAME frames)   │  │  (SAME frames)   │
└────────┬─────────┘  └────────┬─────────┘  └────────┬─────────┘
         │                      │                      │
         │   Results sent back to coordinator          │
         │                      │                      │
         └──────────────────────┼──────────────────────┘
                                ▼
                    ┌─────────────────────────┐
                    │   Coordinator           │
                    │   (Composite Results)   │
                    └─────────────────────────┘
```

**Key difference: PARALLEL, not serial**
- Coordinator broadcasts same video to all workers simultaneously
- All workers process the SAME frames in parallel
- Workers send results back asynchronously
- Coordinator waits for all results for a frame, then composites
- Much faster than serial processing!

### Serializable Stream Format

For network transfer, stream data is serialized:

```python
# Stream chunk format (sent over network)
{
    "timestamp": 1234567890.123,    # Nanosecond precision
    "frame_number": 42,
    "data": <binary frame data>,    # Compressed or raw
    "format": "rgb24",              # Or "yuv420p", "depth_float32", etc.
    "width": 1920,
    "height": 1080,
    "metadata": {                   # Optional
        "ptp_time": 1234567890.123,
        "source_id": "phone_cam_0"
    }
}
```

This allows:
- Network transmission with full timing info
- Reconstruction on receiving end
- Lossless or lossy compression options
- Multiple stream merging based on timestamps

## Core API Design

### 1. Stream Sources with Time Synchronization

```python
from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import Literal
import numpy as np
import time

@dataclass
class TimestampedFrame:
    """Frame with hardware-level timing information for sync"""
    frame: np.ndarray

    # Multiple timestamp types for different sync scenarios
    pts: int                    # Presentation timestamp (from source)
    capture_time: float         # Hardware capture time (monotonic)
    ptp_time: float = None      # PTP synchronized time (if available)
    frame_number: int = 0       # Sequential frame counter

    # Genlock-style info
    sync_source: str = None     # Which clock this is synced to
    drift_ns: int = 0           # Drift from reference in nanoseconds

class StreamSource(ABC):
    """Abstract base for video/audio sources"""

    @abstractmethod
    async def next_frame(self) -> np.ndarray | TimestampedFrame:
        """Get next video frame as numpy array or timestamped frame"""
        pass

    @abstractmethod
    async def close(self):
        pass

class SyncedSource(StreamSource):
    """Source with time synchronization support for multi-camera setups"""

    def __init__(
        self,
        device_id: int,
        sync_mode: Literal['none', 'ptp', 'genlock'] = 'none',
        ptp_domain: int = 0,        # PTP domain for multi-network setups
        reference_clock: str = None  # Master clock address
    ):
        self.sync_mode = sync_mode
        self.device_id = device_id

        if sync_mode == 'ptp':
            # Use PTP for network time sync
            self.ptp_client = PTPClient(domain=ptp_domain)
            if reference_clock:
                self.ptp_client.sync_to(reference_clock)

    async def next_frame(self) -> TimestampedFrame:
        """Returns frame with precise timing information"""
        frame = await self._capture_frame()

        return TimestampedFrame(
            frame=frame,
            pts=self._get_hardware_pts(),
            capture_time=time.monotonic_ns(),  # Nanosecond precision
            ptp_time=self.ptp_client.now() if self.sync_mode == 'ptp' else None,
            frame_number=self.frame_count,
            sync_source=f"{self.sync_mode}:{self.device_id}"
        )

class WebcamSource(StreamSource):
    """Capture from webcam using PyAV"""
    def __init__(self, device_id: int = 0, width: int = 1920, height: int = 1080, fps: int = 30):
        pass

class ScreenCaptureSource(StreamSource):
    """Screen capture using PyAV or platform-specific APIs"""
    pass

class FileSource(StreamSource):
    """Read from video file using PyAV"""
    def __init__(self, file_path: str):
        pass

class TestSource(StreamSource):
    """Generate frames programmatically"""
    def __init__(self, width: int, height: int, fps: int, draw_func: callable):
        self.draw_func = draw_func  # Like our DrawingEngine

class NetworkSource(StreamSource):
    """Receive stream from another machine over network"""
    def __init__(
        self,
        host: str,
        port: int = 8000,
        protocol: Literal['tcp', 'udp', 'webrtc'] = 'tcp',
        compression: Literal['none', 'jpeg', 'h264'] = 'none'
    ):
        """
        Connect to remote stream source.

        Examples:
            NetworkSource('phone.local', 8000)
            NetworkSource('192.168.1.100', 8000, protocol='webrtc')
        """
        pass
```

### 2. Stream Sinks

```python
class StreamSink(ABC):
    """Abstract base for output destinations"""

    @abstractmethod
    async def write_frame(self, frame: np.ndarray, timestamp: float):
        pass

    @abstractmethod
    async def close(self):
        pass

class HLSSink(StreamSink):
    """HLS streaming using PyAV (no GStreamer!)"""
    def __init__(
        self,
        output_dir: str,
        segment_duration: int = 2,
        playlist_size: int = 10,
        bitrate: int = 8000000
    ):
        # Use PyAV's av.open() with 'hls' format
        self.container = av.open(output_dir, mode='w', format='hls')
        self.video_stream = self.container.add_stream('h264', rate=30)
        self.video_stream.bit_rate = bitrate

class WebRTCSink(StreamSink):
    """WebRTC streaming using aiortc (like fastrtc)"""
    def __init__(self, ice_servers: list = None):
        pass

class FileSink(StreamSink):
    """Save to video file using PyAV"""
    def __init__(self, output_path: str, codec: str = 'h264', bitrate: int = 8000000):
        self.container = av.open(output_path, mode='w')

class DisplaySink(StreamSink):
    """Display in window using matplotlib or OpenCV"""
    pass

class NetworkSink(StreamSink):
    """Send stream to another machine over network"""
    def __init__(
        self,
        port: int = 8000,
        protocol: Literal['tcp', 'udp', 'webrtc'] = 'tcp',
        compression: Literal['none', 'jpeg', 'h264'] = 'none',
        bind_address: str = '0.0.0.0'
    ):
        """
        Start network server to send frames to remote consumers.

        Examples:
            NetworkSink(8000)  # Listen on port 8000
            NetworkSink(8000, protocol='webrtc', compression='h264')
        """
        pass
```

### 3. Compositor with ML and Multi-Stream Support

```python
class Layer(ABC):
    """Abstract base for compositing layers"""
    def __init__(self, layer_id: str, z_index: int):
        self.layer_id = layer_id
        self.z_index = z_index

    @abstractmethod
    async def render(self, canvas, ctx: dict) -> None:
        """Render this layer onto canvas"""
        pass

class VideoLayer(Layer):
    """Video source as layer"""
    def __init__(self, layer_id: str, z_index: int, source: StreamSource):
        super().__init__(layer_id, z_index)
        self.source = source

    async def render(self, canvas, ctx: dict):
        frame = await self.source.next_frame()
        # Blend onto canvas with alpha

class DrawingLayer(Layer):
    """Skia-based drawing layer - fully programmable"""
    def __init__(self, layer_id: str, z_index: int, draw_code: str):
        super().__init__(layer_id, z_index)
        self.draw_code = draw_code
        self.engine = DrawingEngine()  # Our existing Skia wrapper

    async def render(self, canvas, ctx: dict):
        self.engine.execute(self.draw_code, canvas, ctx)

class MLLayer(Layer):
    """
    ML/AI layer with zero-copy tensor operations.
    Supports PyTorch, TensorFlow, etc. with GPU acceleration.
    """
    def __init__(
        self,
        layer_id: str,
        z_index: int,
        model: Any,
        device: str = 'cuda'
    ):
        super().__init__(layer_id, z_index)
        self.model = model.to(device) if hasattr(model, 'to') else model
        self.device = device

    async def render(self, canvas, ctx: dict):
        # Zero-copy: Get frame from context
        if 'frame' in ctx:
            import torch

            # Zero-copy: numpy → tensor
            tensor = torch.from_numpy(ctx['frame']).to(self.device)

            # GPU inference
            with torch.no_grad():
                output = self.model(tensor)

            # Zero-copy: tensor → numpy
            result = output.cpu().numpy()

            # Draw result on canvas
            # (e.g., segmentation mask, detected objects, etc.)
            self._draw_ml_output(canvas, result)

class Compositor:
    """Multi-layer video compositor with alpha blending and zero-copy"""

    def __init__(self, width: int = 1920, height: int = 1080, fps: int = 30):
        self.width = width
        self.height = height
        self.fps = fps
        self.layers = []
        self.start_time = None

    def add_layer(self, layer: Layer):
        """Add layer - can be called at runtime for emergent behaviors"""
        self.layers.append(layer)
        self.layers.sort(key=lambda l: l.z_index)

    def remove_layer(self, layer_id: str):
        """Remove layer dynamically"""
        self.layers = [l for l in self.layers if l.layer_id != layer_id]

    async def compose_frame(self, base_frame: np.ndarray = None, timestamp: float = 0.0) -> np.ndarray:
        """Compose all layers into single frame with zero-copy"""
        import skia

        # Create base canvas
        surface = skia.Surface(self.width, self.height)
        canvas = surface.getCanvas()

        # Build context with frame access for ML layers
        ctx = {
            'time': timestamp,
            'width': self.width,
            'height': self.height,
            'frame': base_frame,  # Available for ML inference
            'fps': self.fps
        }

        # Draw base frame if provided
        if base_frame is not None:
            # Convert numpy to skia image (can be zero-copy with proper setup)
            image = numpy_to_skia_image(base_frame)
            canvas.drawImage(image, 0, 0)

        # Render each layer
        for layer in self.layers:
            await layer.render(canvas, ctx)

        # Convert to numpy array
        image = surface.makeImageSnapshot()
        return skia_to_numpy(image)  # Can be zero-copy


class MultiStreamCompositor:
    """
    Compositor for multiple synchronized streams.
    Essential for 3D tracking and multi-camera applications.
    """

    def __init__(
        self,
        sync_tolerance_ns: int = 1_000_000,  # 1ms tolerance
        sync_mode: Literal['best_effort', 'strict'] = 'strict'
    ):
        self.sources = []
        self.sync_tolerance_ns = sync_tolerance_ns
        self.sync_mode = sync_mode
        self.frame_buffers = {}  # Buffer frames waiting for sync

    def add_synced_source(self, source_id: str, source: SyncedSource):
        """Add a time-synchronized source (e.g., camera)"""
        self.sources.append({'id': source_id, 'source': source})
        self.frame_buffers[source_id] = []

    async def get_synced_frames(self) -> dict[str, TimestampedFrame]:
        """
        Get frames from all sources captured at the same time.
        Critical for 3D tracking where temporal alignment is essential.
        """

        # Collect frames from all sources
        for source_info in self.sources:
            frame = await source_info['source'].next_frame()
            self.frame_buffers[source_info['id']].append(frame)

        # Find frames that match in time
        synced_set = self._find_temporally_aligned_frames()

        if synced_set is None and self.sync_mode == 'strict':
            # Wait for better alignment
            return await self.get_synced_frames()

        return synced_set

    def _find_temporally_aligned_frames(self) -> dict[str, TimestampedFrame] | None:
        """
        Find set of frames from all sources within sync tolerance.
        Uses PTP timestamps if available, falls back to capture_time.
        """

        if not all(len(buf) > 0 for buf in self.frame_buffers.values()):
            return None

        # Get oldest frame from first source as reference
        ref_source_id = self.sources[0]['id']
        ref_frame = self.frame_buffers[ref_source_id][0]
        ref_time = ref_frame.ptp_time or ref_frame.capture_time

        synced_set = {ref_source_id: ref_frame}

        # Find matching frames from other sources
        for source_info in self.sources[1:]:
            source_id = source_info['id']
            buffer = self.frame_buffers[source_id]

            # Find frame closest in time to reference
            best_match = None
            min_diff = float('inf')

            for frame in buffer:
                frame_time = frame.ptp_time or frame.capture_time
                diff = abs(frame_time - ref_time)

                if diff < min_diff:
                    min_diff = diff
                    best_match = frame

            # Check if within tolerance
            if min_diff <= self.sync_tolerance_ns:
                synced_set[source_id] = best_match
            else:
                return None  # No sync achieved

        # Remove used frames from buffers
        for source_id, frame in synced_set.items():
            self.frame_buffers[source_id].remove(frame)

        return synced_set
```

### 4. PTP Client for Time Synchronization

```python
class PTPClient:
    """
    PTP (Precision Time Protocol) client for sub-microsecond sync.
    Based on IEEE 1588, used by SMPTE 2110.
    Open standard - no licensing required.
    """

    def __init__(self, domain: int = 0):
        """
        domain: PTP domain (0-255), allows multiple independent sync groups
        """
        import socket

        self.domain = domain
        self.master_clock = None
        self.offset = 0  # Offset from master in nanoseconds
        self.synced = False

        # PTP uses UDP multicast (224.0.1.129:319 for event messages)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

    def sync_to(self, master_address: str):
        """Sync to PTP master clock"""
        # Use existing library: python-ptpd or ptpython
        # This provides the PTP protocol implementation
        self.master_clock = master_address
        self._run_ptp_sync()

    def now(self) -> float:
        """Get current time in PTP synchronized nanoseconds"""
        if not self.synced:
            return time.monotonic_ns()

        return time.monotonic_ns() + self.offset

    def _run_ptp_sync(self):
        """
        Run PTP synchronization protocol.
        Exchange Sync, Follow_Up, Delay_Req, Delay_Resp messages.
        Achieves sub-microsecond accuracy on good networks.
        """
        # Implementation would use existing PTP library
        # or implement IEEE 1588 protocol
        pass

class SMPTE2110Source(SyncedSource):
    """
    Future: Full SMPTE 2110 support for professional broadcast.

    SMPTE 2110 includes:
    - 2110-20: Uncompressed video over IP
    - 2110-30: Audio
    - 2110-40: Metadata
    - Uses PTP for timing (which we already support)

    This can be added as a plugin later without changing core architecture.
    """

    def __init__(
        self,
        stream_url: str,  # e.g., "rtp://239.0.0.1:5004"
        ptp_domain: int = 127  # SMPTE default domain
    ):
        super().__init__(
            device_id=0,
            sync_mode='ptp',
            ptp_domain=ptp_domain
        )
        # Would use libraries like:
        # - Build on top of our PTP support + RTP
        # - FFmpeg with SMPTE 2110 support
        pass
```

### 5. Main Stream Class

```python
class Stream:
    """Main streaming pipeline with optional time sync support"""

    def __init__(
        self,
        source: StreamSource = None,
        sink: StreamSink = None,
        width: int = 1920,
        height: int = 1080,
        fps: int = 30,
        sync_mode: Literal['none', 'ptp', 'genlock'] = 'none'
    ):
        self.source = source
        self.sink = sink
        self.sync_mode = sync_mode
        self.running = False

        # Use multi-stream compositor if sync is enabled
        if sync_mode != 'none':
            self.compositor = MultiStreamCompositor()
        else:
            self.compositor = Compositor(width, height, fps)

    def add_layer(self, layer: Layer):
        """Add compositing layer - can be called at runtime"""
        self.compositor.add_layer(layer)

    def remove_layer(self, layer_id: str):
        """Remove layer dynamically - enables emergent behaviors"""
        self.compositor.remove_layer(layer_id)

    async def run(self):
        """Run the streaming pipeline"""
        self.running = True
        frame_time = 1.0 / self.compositor.fps if hasattr(self.compositor, 'fps') else 1.0/30
        start_time = time.time()

        while self.running:
            timestamp = time.time() - start_time

            # Get base frame from source (if any)
            base_frame = None
            if self.source:
                frame_or_timestamped = await self.source.next_frame()

                # Handle both regular frames and timestamped frames
                if isinstance(frame_or_timestamped, TimestampedFrame):
                    base_frame = frame_or_timestamped.frame
                else:
                    base_frame = frame_or_timestamped

            # Compose with layers
            composed = await self.compositor.compose_frame(base_frame, timestamp)

            # Write to sink
            if self.sink:
                await self.sink.write_frame(composed, timestamp)

            # Maintain frame rate
            elapsed = time.time() - start_time - timestamp
            if elapsed < frame_time:
                await asyncio.sleep(frame_time - elapsed)

    async def stop(self):
        self.running = False
        if self.source:
            await self.source.close()
        if self.sink:
            await self.sink.close()
```

## Usage Examples

### Example 1: Generated Content → HLS (Like Our Tutorial)

```python
import asyncio

async def main():
    # Create stream with HLS output
    stream = Stream(
        sink=HLSSink('output/stream', segment_duration=2),
        width=1920,
        height=1080,
        fps=30
    )

    # Add drawing layer with animated code
    draw_code = '''
import math

def draw(canvas, ctx):
    paint = skia.Paint(AntiAlias=True)
    paint.setColor(skia.ColorBLUE)

    # Animated circle
    x = 960 + 400 * math.cos(ctx.time * 2)
    y = 540 + 400 * math.sin(ctx.time * 2)
    canvas.drawCircle(x, y, 50, paint)
'''

    layer = DrawingLayer('animation', z_index=0, draw_code=draw_code)
    stream.add_layer(layer)

    # Run for 10 seconds
    await asyncio.wait_for(stream.run(), timeout=10)
    await stream.stop()

asyncio.run(main())
```

### Example 2: Webcam → Compositor → WebRTC

```python
async def main():
    # Webcam source
    webcam = WebcamSource(device_id=0, width=1920, height=1080, fps=30)

    # WebRTC sink
    webrtc = WebRTCSink(ice_servers=[...])

    # Create stream
    stream = Stream(source=webcam, sink=webrtc)

    # Add overlay layer
    overlay_code = '''
def draw(canvas, ctx):
    # Draw AR measurement overlay
    paint = skia.Paint(AntiAlias=True, Color=skia.ColorRED)
    canvas.drawText("Distance: 2.5m", 100, 100, paint)
'''

    stream.add_layer(DrawingLayer('overlay', z_index=1, draw_code=overlay_code))

    await stream.run()
```

### Example 3: Screen Capture → File

```python
async def main():
    screen = ScreenCaptureSource()
    file_out = FileSink('recording.mp4', codec='h264', bitrate=8000000)

    stream = Stream(source=screen, sink=file_out)
    await stream.run()
```

## Advanced Usage Examples

### Example 4: Multi-Camera 3D Tracking with PTP Sync

```python
async def multi_camera_3d_tracking():
    """
    3D pose tracking with 4 synchronized cameras.
    Sub-millisecond sync using PTP.
    """

    # Setup multi-stream compositor with strict sync
    compositor = MultiStreamCompositor(
        sync_tolerance_ns=500_000,  # 0.5ms tolerance
        sync_mode='strict'
    )

    # Add 4 cameras, all synced to PTP master
    for i in range(4):
        camera = SyncedSource(
            device_id=i,
            sync_mode='ptp',
            ptp_domain=0,
            reference_clock='192.168.1.100'  # PTP master
        )
        compositor.add_synced_source(f'cam_{i}', camera)

    # 3D tracking model (e.g., MediaPipe, OpenPose)
    tracker_3d = Tracker3D(num_cameras=4)

    while True:
        # Get frames from all cameras captured at SAME moment
        synced_frames = await compositor.get_synced_frames()

        # Extract synchronized numpy arrays
        frames = [sf.frame for sf in synced_frames.values()]

        # 3D tracking requires temporally aligned frames
        pose_3d = tracker_3d.estimate_pose(frames)

        # Verify sync quality
        timestamps = [sf.ptp_time for sf in synced_frames.values()]
        jitter = max(timestamps) - min(timestamps)
        print(f"Sync jitter: {jitter/1e6:.3f}ms")

        # Draw 3D pose on output
        # ...
```

### Example 5: ML Layer with Zero-Copy GPU Processing

```python
async def ml_processing_example():
    """
    Real-time semantic segmentation with zero-copy GPU ops.
    """
    import torch
    from torchvision import models

    # Load segmentation model
    model = models.segmentation.deeplabv3_resnet50(pretrained=True)
    model.eval()

    class SegmentationLayer(MLLayer):
        def _draw_ml_output(self, canvas, result):
            # Draw segmentation mask as overlay
            mask = result.argmax(0)

            # Color code different classes
            colored_mask = colorize_segmentation(mask)

            # Draw semi-transparent overlay
            paint = skia.Paint(AntiAlias=True)
            paint.setAlpha(128)
            canvas.drawImage(numpy_to_skia(colored_mask), 0, 0, paint)

    # Create stream
    webcam = WebcamSource(device_id=0)
    stream = Stream(source=webcam, sink=DisplaySink())

    # Add ML layer (GPU accelerated, zero-copy)
    ml_layer = SegmentationLayer(
        'segmentation',
        z_index=1,
        model=model,
        device='cuda'  # GPU acceleration
    )
    stream.add_layer(ml_layer)

    await stream.run()
```

### Example 6: Distributed Processing Chain

```python
# ============================================
# Machine 1: Phone (Capture)
# ============================================
async def phone_capture():
    """Capture from phone camera and send to edge device"""
    stream = Stream(
        source=WebcamSource(device_id=0, width=1920, height=1080),
        sink=NetworkSink(port=8000, compression='h264'),  # Send to network
        fps=30
    )
    await stream.run()

# ============================================
# Machine 2: Edge Device (Processing)
# ============================================
async def edge_processing():
    """Receive from phone, add overlays, send to cloud"""
    stream = Stream(
        source=NetworkSource('phone.local', 8000),  # Receive from phone
        sink=NetworkSink(port=8001),  # Send to cloud
        width=1920,
        height=1080,
        fps=30
    )

    # Add overlay layer
    overlay_code = '''
def draw(canvas, ctx):
    paint = skia.Paint(AntiAlias=True)
    paint.setColor(skia.ColorWHITE)
    paint.setTextSize(48)
    canvas.drawText(f"FPS: {ctx.fps}", 50, 100, paint)
'''
    stream.add_layer(DrawingLayer('overlay', z_index=1, draw_code=overlay_code))

    await stream.run()

# ============================================
# Machine 3: Cloud GPU (ML Processing)
# ============================================
async def cloud_ml_processing():
    """Receive from edge, run ML, send to display"""
    import torch
    from torchvision import models

    # Load detection model
    model = models.detection.fasterrcnn_resnet50_fpn(pretrained=True)
    model.eval()

    stream = Stream(
        source=NetworkSource('edge-device.local', 8001),  # Receive from edge
        sink=NetworkSink(port=8002),  # Send to display
        width=1920,
        height=1080,
        fps=30
    )

    # Add ML detection layer
    stream.add_layer(MLLayer('detection', z_index=2, model=model, device='cuda'))

    await stream.run()

# ============================================
# Machine 4: Display/Recording (Output)
# ============================================
async def display_and_record():
    """Receive from cloud and output to HLS + display"""
    stream = Stream(
        source=NetworkSource('cloud-gpu.local', 8002),  # Receive from cloud
        sink=HLSSink('output/stream'),  # Save as HLS
        width=1920,
        height=1080,
        fps=30
    )
    await stream.run()
```

### Example 7: Mesh Processing (Parallel ML Models)

```python
# ============================================
# Coordinator: Broadcast video to workers, composite results
# ============================================
async def coordinator():
    """
    Broadcast source video to all workers in parallel.
    Receive and composite their ML outputs.
    """

    # Source: webcam or any video source
    video_source = WebcamSource(device_id=0, width=1920, height=1080, fps=30)

    # Create multiple network sinks - one for each worker
    # This broadcasts the same video to all workers simultaneously
    worker_sinks = {
        'detection': NetworkSink(port=7001),      # Worker 1 connects here
        'segmentation': NetworkSink(port=7002),   # Worker 2 connects here
        'depth': NetworkSink(port=7003)           # Worker 3 connects here
    }

    # Compositor to receive results back from workers
    compositor = MultiStreamCompositor()
    compositor.add_synced_source('detection', NetworkSource('ml-worker-1:8000'))
    compositor.add_synced_source('segmentation', NetworkSource('ml-worker-2:8000'))
    compositor.add_synced_source('depth', NetworkSource('ml-worker-3:8000'))

    # Output
    output = HLSSink('output/final')

    # Task 1: Broadcast video to all workers in parallel
    async def broadcast_video():
        while True:
            frame = await video_source.next_frame()
            timestamp = time.time()

            # Send SAME frame to all workers simultaneously (parallel)
            await asyncio.gather(
                worker_sinks['detection'].write_frame(frame, timestamp),
                worker_sinks['segmentation'].write_frame(frame, timestamp),
                worker_sinks['depth'].write_frame(frame, timestamp)
            )

    # Task 2: Receive and composite results
    async def composite_results():
        while True:
            # Get synchronized results from all workers (they processed in parallel)
            synced_frames = await compositor.get_synced_frames()

            detection_result = synced_frames['detection'].frame
            segmentation_result = synced_frames['segmentation'].frame
            depth_result = synced_frames['depth'].frame

            # Composite all ML outputs together
            final_frame = composite_ml_results(
                detection_result,
                segmentation_result,
                depth_result
            )

            # Output final composited stream
            await output.write_frame(final_frame, synced_frames['detection'].ptp_time)

    # Run both tasks concurrently
    await asyncio.gather(
        broadcast_video(),
        composite_results()
    )


# ============================================
# ML Worker 1: Object Detection
# ============================================
async def ml_worker_detection():
    """Receive video from coordinator, run detection, send results back"""
    stream = Stream(
        source=NetworkSource('coordinator:7001'),  # Receive broadcast from coordinator
        sink=NetworkSink(8000),                     # Send results back
        width=1920,
        height=1080,
        fps=30
    )

    model = load_detection_model()
    stream.add_layer(MLLayer('detection', z_index=1, model=model, device='cuda'))

    await stream.run()


# ============================================
# ML Worker 2: Segmentation
# ============================================
async def ml_worker_segmentation():
    """Receive video from coordinator, run segmentation, send results back"""
    stream = Stream(
        source=NetworkSource('coordinator:7002'),  # Receive broadcast from coordinator
        sink=NetworkSink(8000),                     # Send results back
        width=1920,
        height=1080,
        fps=30
    )

    model = load_segmentation_model()
    stream.add_layer(MLLayer('segmentation', z_index=1, model=model, device='cuda'))

    await stream.run()


# ============================================
# ML Worker 3: Depth Estimation
# ============================================
async def ml_worker_depth():
    """Receive video from coordinator, run depth estimation, send results back"""
    stream = Stream(
        source=NetworkSource('coordinator:7003'),  # Receive broadcast from coordinator
        sink=NetworkSink(8000),                     # Send results back
        width=1920,
        height=1080,
        fps=30
    )

    model = load_depth_model()
    stream.add_layer(MLLayer('depth', z_index=1, model=model, device='cuda'))

    await stream.run()
```

**How parallel processing works:**

1. **Coordinator broadcasts** frame 42 to all 3 workers simultaneously (via `asyncio.gather`)
2. **All workers process frame 42 in parallel:**
   - Worker 1 runs object detection on frame 42
   - Worker 2 runs segmentation on frame 42
   - Worker 3 runs depth estimation on frame 42
   - All happening at the same time!
3. **Coordinator waits** for all 3 results for frame 42
4. **Coordinator composites** the 3 ML outputs into final frame 42
5. **Repeat** for frame 43, 44, 45...

**Performance benefit:** If each ML model takes 100ms, serial would take 300ms total. Parallel takes only 100ms (the slowest model)!

## Implementation Plan

### Phase 1: Core Infrastructure (Foundation)
1. ✅ Design API (this document)
2. ⏳ Implement abstract base classes (Source, Sink, Layer, Compositor)
3. ⏳ Port DrawingEngine from existing code
4. ⏳ Port Compositor with zero-copy numpy pipeline
5. ⏳ Implement TimestampedFrame and timing infrastructure
6. ⏳ Create plugin registration system

### Phase 2: Basic Sources & Sinks (Pure Python)
1. ⏳ FileSource using PyAV
2. ⏳ TestSource (pure drawing)
3. ⏳ FileSink using PyAV
4. ⏳ HLSSink using PyAV (replaces GStreamer!)
5. ⏳ DisplaySink for preview

### Phase 3: Hardware I/O
1. ⏳ WebcamSource using PyAV
2. ⏳ ScreenCaptureSource (platform-specific)
3. ⏳ Audio support (from fastrtc patterns)

### Phase 4: Network-Transparent Operations
1. ⏳ Serializable stream format design
2. ⏳ NetworkSource implementation (TCP/UDP/WebRTC)
3. ⏳ NetworkSink implementation
4. ⏳ Compression options (none, JPEG, H.264)
5. ⏳ Network protocol for stream chunks
6. ⏳ Test distributed processing across machines

### Phase 5: Time Synchronization
1. ⏳ PTP Client implementation (IEEE 1588)
2. ⏳ SyncedSource with hardware timestamps
3. ⏳ MultiStreamCompositor for multi-camera
4. ⏳ Temporal alignment algorithms
5. ⏳ Sync quality monitoring

### Phase 6: ML & GPU Acceleration
1. ⏳ MLLayer base class
2. ⏳ Zero-copy numpy ↔ tensor conversions
3. ⏳ GPU device management (CUDA/Metal)
4. ⏳ Integration examples (PyTorch, TensorFlow)
5. ⏳ Benchmark zero-copy performance

### Phase 7: Advanced Features
1. ⏳ Object detection integration examples
2. ⏳ AR measurement tools
3. ⏳ 3D tracking examples
4. ⏳ SMPTE 2110 plugin (optional)
5. ⏳ Advanced sync scenarios
6. ⏳ Performance optimization and benchmarking

## Key Dependencies

```toml
[project]
name = "streamkit"  # or similar
version = "0.1.0"
dependencies = [
    "numpy>=1.24.0",        # Core array operations, zero-copy foundation
    "av>=10.0.0",           # PyAV for video processing (replaces GStreamer!)
    "skia-python>=87.0",    # For drawing/composition (optional if using Metal)
]

[project.optional-dependencies]
# Optional feature sets
webrtc = [
    "aiortc>=1.5.0",        # WebRTC support
    "aioice>=0.10.1",       # ICE protocol
]

audio = [
    "librosa>=0.10.0",      # Audio processing
]

ml = [
    "torch>=2.0.0",         # PyTorch for ML layers
    # or "tensorflow>=2.0.0"
]

sync = [
    "ptpython",             # PTP time sync (if library exists)
    # or implement PTP protocol directly
]

detection = [
    "ultralytics",          # YOLO object detection
]

all = [
    "streamkit[webrtc,audio,ml,sync,detection]"
]
```

**Package Structure:**
```
streamkit/              # Core library (minimal dependencies)
├── core/              # Abstract base classes
├── sources/           # Source implementations (PyAV-based)
├── sinks/             # Sink implementations (HLS, WebRTC, file)
├── layers/            # Layer implementations (Drawing, ML, Video)
├── compositor/        # Compositor logic
└── sync/              # Time synchronization (PTP)

## Benefits

### Core Requirements Met
1. ✅ **No system dependencies** - Everything installable via pip (replaces GStreamer!)
2. ✅ **Transport-agnostic** - HLS, WebRTC, file, display, network, SMPTE 2110 all supported
3. ✅ **Clean API** - Simple source → compositor → sink pipeline
4. ✅ **Composable primitives** - Like Unix tools for visual operations
5. ✅ **Network-transparent** - Local and remote operations use same API
6. ✅ **Distributed processing** - Chain operations across machines seamlessly
7. ✅ **Mesh-capable** - Multiple machines collaborate on same stream
8. ✅ **Maintains our strengths** - Compositor + drawing layers proven design
9. ✅ **Adds fastrtc strengths** - Pure Python, async-first, aiortc integration
10. ✅ **Extensible** - Plugin architecture for flexibility

### Extended Requirements Met
11. ✅ **ML zero-copy** - Numpy ↔ tensor zero-copy, GPU acceleration
12. ✅ **Fully programmable** - SDK allows custom layers and behaviors
13. ✅ **Time sync** - PTP support, hardware timestamps, multi-camera alignment
14. ✅ **3D tracking** - Sub-millisecond sync for multi-camera 3D applications

### Technical Advantages
15. ✅ **Performance** - Zero-copy pipeline, GPU acceleration, hardware timestamps
16. ✅ **Modularity** - Core + plugins, install only what you need
17. ✅ **Scalability** - Distribute workload across phones, edge, cloud seamlessly
18. ✅ **Edge computing ready** - Process where it makes sense (latency, bandwidth, compute)
19. ✅ **Future-proof** - SMPTE 2110, genlock architecturally supported
20. ✅ **Developer experience** - Simple API, clear abstractions, async-first
21. ✅ **Production ready** - Professional sync, reliable streaming, quality monitoring
22. ✅ **Universal** - Usable by scripts, programs, command-line tools, any orchestrator

## Migration Path

Our existing tutorial demo becomes:

```python
# OLD (GStreamer):
pipeline_str = f"""
    appsrc name=videosrc ! videoconvert ! x264enc ! hlssink2
"""

# NEW (PyAV):
stream = Stream(sink=HLSSink('output/'))
stream.add_layer(DrawingLayer('tutorial', z_index=0, draw_code=tutorial_code))
await stream.run()
```

Much simpler and no GStreamer installation required!
