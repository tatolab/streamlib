# Messages

Messages are the data containers that flow through ports. Like data structures in Unix pipes, they carry the actual payload between handlers.

## VideoFrame

The primary message type for video data.

```python
@dataclass
class VideoFrame:
    """A single video frame with metadata."""

    data: Union[np.ndarray, torch.Tensor, Any]  # Frame data (CPU or GPU)
    timestamp: float                             # Time in seconds
    frame_number: int                            # Sequential frame counter
    width: int                                   # Frame width in pixels
    height: int                                  # Frame height in pixels
    metadata: Dict[str, Any] = field(default_factory=dict)  # Additional info
```

### Creating VideoFrames

```python
from streamlib import VideoFrame
import numpy as np

# CPU frame (numpy array)
frame = VideoFrame(
    data=np.zeros((1080, 1920, 3), dtype=np.uint8),  # RGB image
    timestamp=0.033,
    frame_number=1,
    width=1920,
    height=1080
)

# GPU frame (torch tensor)
import torch
gpu_data = torch.zeros((1080, 1920, 3), device='mps')
frame = VideoFrame(
    data=gpu_data,
    timestamp=0.033,
    frame_number=1,
    width=1920,
    height=1080,
    metadata={'memory': 'gpu', 'backend': 'mps'}
)

# With custom metadata
frame = VideoFrame(
    data=image_array,
    timestamp=0.033,
    frame_number=1,
    width=1920,
    height=1080,
    metadata={
        'source': 'camera',
        'exposure': 0.016,
        'iso': 400
    }
)
```

### Accessing Frame Data

```python
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Access properties
        print(f"Frame {frame.frame_number} @ {frame.timestamp:.3f}s")
        print(f"Resolution: {frame.width}x{frame.height}")

        # Access data
        if isinstance(frame.data, np.ndarray):
            # CPU processing
            gray = cv2.cvtColor(frame.data, cv2.COLOR_RGB2GRAY)
        elif isinstance(frame.data, torch.Tensor):
            # GPU processing
            gray = frame.data.mean(dim=2)

        # Access metadata
        if 'source' in frame.metadata:
            print(f"Source: {frame.metadata['source']}")
```

### Copying Frames

```python
# Create a new frame with modified data
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Process data
        processed_data = self.process_image(frame.data)

        # Create new frame (preserves metadata)
        output_frame = VideoFrame(
            data=processed_data,
            timestamp=frame.timestamp,
            frame_number=frame.frame_number,
            width=frame.width,
            height=frame.height,
            metadata=frame.metadata  # Copy metadata
        )
        self.outputs['video'].write(output_frame)
```

## TimedTick

Clock tick message sent by runtime to all handlers.

```python
@dataclass
class TimedTick:
    """Clock tick with timing information."""

    timestamp: float      # Monotonic time in seconds
    frame_number: int     # Sequential frame counter
    clock_id: str         # Clock identifier
```

### Using Ticks

```python
async def process(self, tick: TimedTick):
    """Process called every tick."""
    print(f"Tick {tick.frame_number} @ {tick.timestamp:.3f}s")

    # Use tick timing for outputs
    frame = VideoFrame(
        data=generated_image,
        timestamp=tick.timestamp,
        frame_number=tick.frame_number,
        width=self.width,
        height=self.height
    )
    self.outputs['video'].write(frame)
```

## AudioFrame

For audio data (future).

```python
@dataclass
class AudioFrame:
    """A chunk of audio samples."""

    data: Union[np.ndarray, torch.Tensor]  # Audio samples
    timestamp: float                        # Time in seconds
    sample_number: int                      # Sequential sample counter
    sample_rate: int                        # Samples per second
    channels: int                           # Number of channels
    metadata: Dict[str, Any] = field(default_factory=dict)
```

## Custom Messages

You can create custom message types for specialized data:

```python
@dataclass
class DetectionFrame:
    """Object detection results."""

    detections: List[BoundingBox]  # Detected objects
    timestamp: float               # Time in seconds
    frame_number: int              # Corresponding video frame
    confidence_threshold: float    # Detection threshold used
    metadata: Dict[str, Any] = field(default_factory=dict)
```

### Using Custom Messages

```python
class ObjectDetectorHandler(StreamHandler):
    def __init__(self):
        super().__init__()
        # Input: video frames
        self.inputs['video'] = VideoInput('video', capabilities=['gpu'])

        # Output: detection results (port_type='data')
        self.outputs['detections'] = StreamOutput(
            'detections',
            port_type='data',
            capabilities=['cpu']  # Detections are CPU data structures
        )

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Run detection
            detections = self.detect_objects(frame.data)

            # Output detection message
            result = DetectionFrame(
                detections=detections,
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                confidence_threshold=self.threshold
            )
            self.outputs['detections'].write(result)
```

## Memory Management

### Zero-Copy Principle

Messages should reference data, not copy it:

```python
# ✅ Good: Reference existing data
frame = VideoFrame(
    data=gpu_tensor,  # Reference (no copy)
    timestamp=tick.timestamp,
    frame_number=tick.frame_number,
    width=self.width,
    height=self.height
)

# ❌ Bad: Unnecessary copy
frame = VideoFrame(
    data=gpu_tensor.clone(),  # Copies data!
    ...
)
```

### GPU Memory

Keep GPU data on GPU as long as possible:

```python
# ✅ Good: Process stays on GPU
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # GPU operation (no CPU transfer)
        result = self.gpu_process(frame.data)
        self.outputs['video'].write(VideoFrame(data=result, ...))

# ❌ Bad: Unnecessary GPU→CPU→GPU transfers
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Transfer to CPU
        cpu_data = frame.data.cpu().numpy()
        # Process on CPU
        result = self.cpu_process(cpu_data)
        # Transfer back to GPU
        gpu_result = torch.from_numpy(result).to('mps')
        self.outputs['video'].write(VideoFrame(data=gpu_result, ...))
```

## Metadata Conventions

Common metadata keys:

```python
metadata = {
    # Memory space
    'memory': 'cpu',        # or 'gpu'
    'backend': 'mps',       # or 'cuda', 'metal'

    # Source
    'source': 'camera',     # or 'file', 'pattern', 'synthetic'
    'device': 'Live Camera',

    # Processing
    'processed_by': 'blur-filter',
    'kernel_size': 15,

    # Camera info
    'exposure': 0.016,
    'iso': 400,
    'focal_length': 24.0,

    # Format info
    'format': 'rgb',        # or 'rgba', 'yuv', 'bgr'
    'dtype': 'uint8',       # or 'float32'
}
```

## Best Practices

1. **Use VideoFrame for images** - Standard container for video data
2. **Preserve timestamps** - Pass through from input to output
3. **Zero-copy where possible** - Reference data, don't copy
4. **Add metadata for debugging** - Helps track data flow
5. **Match tick timing** - Use `tick.timestamp` and `tick.frame_number` for generated frames

## See Also

- [Ports](ports.md) - How messages flow through ports
- [StreamHandler](handler.md) - Processing messages
- [Runtime](runtime.md) - Message timing and distribution
