"""
Message types for inter-handler communication.

Messages are stored in ring buffers and passed between handlers.
All messages include timing information for synchronization.

Message types:
- VideoFrame: Video frame data (WebGPU textures ONLY - no numpy/pytorch)
- AudioBuffer: Audio sample buffer (WebGPU buffers ONLY)
- KeyEvent: Keyboard input event
- MouseEvent: Mouse input event
- DataMessage: Generic data message

IMPORTANT: All video/audio data uses WebGPU. No NumPy, no PyTorch.
Data stays on GPU throughout the pipeline.
"""

from dataclasses import dataclass
from typing import Optional, Any, Dict, TYPE_CHECKING, Literal

if TYPE_CHECKING:
    from .gpu.context import GPUContext


@dataclass
class VideoFrame:
    """
    Video frame message.

    IMPORTANT: data is a WebGPU texture (wgpu.GPUTexture).
    No NumPy arrays, no PyTorch tensors - GPU only!

    Attributes:
        data: WebGPU texture (wgpu.GPUTexture) - RGBA8 format
        timestamp: Absolute timestamp in seconds (from tick)
        frame_number: Monotonic frame counter (from tick)
        width: Frame width in pixels
        height: Frame height in pixels
        metadata: Optional metadata dict (format info, etc.)
    """
    data: Any  # wgpu.GPUTexture (using Any to avoid import dependency)
    timestamp: float
    frame_number: int
    width: int
    height: int
    metadata: Optional[Dict[str, Any]] = None

    def __post_init__(self):
        """Validate frame data (WebGPU textures/buffers only)."""
        # Detect data type
        data_type = type(self.data).__name__

        # WebGPU texture validation (GPUTexture from wgpu)
        if 'Texture' in data_type or 'GPUTexture' in data_type:
            # WebGPU texture - validate dimensions if accessible
            if hasattr(self.data, 'width') and hasattr(self.data, 'height'):
                # Some WebGPU implementations expose dimensions as properties
                if self.data.width != self.width or self.data.height != self.height:
                    raise ValueError(
                        f"VideoFrame dimensions mismatch: texture {self.data.width}x{self.data.height} "
                        f"!= specified {self.width}x{self.height}"
                    )
            # Valid WebGPU texture
            return

        # WebGPU buffer validation (GPUBuffer from wgpu)
        if 'Buffer' in data_type or 'GPUBuffer' in data_type:
            # WebGPU buffer - trust width/height parameters
            # Buffers are opaque GPU memory, can't inspect dimensions
            # Runtime will validate buffer size matches width*height*4 (RGBA8)
            return

        # If we get here, data is not a WebGPU resource
        raise TypeError(
            f"VideoFrame.data must be a WebGPU texture or buffer, got {data_type}. "
            f"streamlib is GPU-only - no NumPy or PyTorch support. "
            f"See docs on WebGPU-first architecture."
        )

    @classmethod
    def create_test_pattern(
        cls,
        gpu_ctx: 'GPUContext',
        width: int,
        height: int,
        timestamp: float,
        frame_number: int = 0,
        pattern: Literal['smpte_bars', 'checkerboard', 'gradient', 'noise'] = 'smpte_bars',
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'VideoFrame':
        """
        Create a VideoFrame with a test pattern.

        Args:
            gpu_ctx: GPU context for texture creation
            width: Frame width
            height: Frame height
            timestamp: Frame timestamp
            frame_number: Frame number in sequence
            pattern: Type of test pattern
            metadata: Optional metadata

        Returns:
            VideoFrame with test pattern texture

        Example:
            frame = VideoFrame.create_test_pattern(
                gpu_ctx, 1920, 1080, tick.timestamp, tick.frame_number
            )
        """
        texture = gpu_ctx.utils.create_test_pattern(width, height, pattern)
        return cls(
            data=texture,
            timestamp=timestamp,
            frame_number=frame_number,
            width=width,
            height=height,
            metadata=metadata
        )

    @classmethod
    def create_solid_color(
        cls,
        gpu_ctx: 'GPUContext',
        width: int,
        height: int,
        timestamp: float,
        frame_number: int = 0,
        color: tuple = (0, 0, 0, 255),
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'VideoFrame':
        """
        Create a VideoFrame with a solid color.

        Args:
            gpu_ctx: GPU context for texture creation
            width: Frame width
            height: Frame height
            timestamp: Frame timestamp
            frame_number: Frame number in sequence
            color: RGBA color tuple (0-255 per component)
            metadata: Optional metadata

        Returns:
            VideoFrame with solid color texture

        Example:
            # Create red frame
            frame = VideoFrame.create_solid_color(
                gpu_ctx, 640, 480, tick.timestamp,
                color=(255, 0, 0, 255)
            )
        """
        texture = gpu_ctx.utils.create_solid_color(width, height, color)
        return cls(
            data=texture,
            timestamp=timestamp,
            frame_number=frame_number,
            width=width,
            height=height,
            metadata=metadata
        )

    @classmethod
    def create_from_texture(
        cls,
        texture: Any,  # wgpu.GPUTexture
        timestamp: float,
        frame_number: int = 0,
        width: Optional[int] = None,
        height: Optional[int] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'VideoFrame':
        """
        Create a VideoFrame from an existing GPU texture.

        Args:
            texture: WebGPU texture
            timestamp: Frame timestamp
            frame_number: Frame number in sequence
            width: Frame width (auto-detected if None)
            height: Frame height (auto-detected if None)
            metadata: Optional metadata

        Returns:
            VideoFrame wrapping the texture

        Example:
            frame = VideoFrame.create_from_texture(
                my_texture, tick.timestamp, tick.frame_number
            )
        """
        # Try to auto-detect dimensions from texture
        if width is None or height is None:
            if hasattr(texture, 'size'):
                width = texture.size[0]
                height = texture.size[1]
            elif hasattr(texture, 'width') and hasattr(texture, 'height'):
                width = texture.width
                height = texture.height
            else:
                raise ValueError("Cannot auto-detect texture dimensions, please provide width and height")

        return cls(
            data=texture,
            timestamp=timestamp,
            frame_number=frame_number,
            width=width,
            height=height,
            metadata=metadata
        )

    def clone_with_texture(
        self,
        new_texture: Any,  # wgpu.GPUTexture
        timestamp: Optional[float] = None
    ) -> 'VideoFrame':
        """
        Create a new VideoFrame with a different texture but same metadata.

        Useful for effects that modify frame data but preserve timing.

        Args:
            new_texture: New GPU texture
            timestamp: Optional new timestamp (uses original if None)

        Returns:
            New VideoFrame with replaced texture

        Example:
            # Apply effect and create new frame
            processed_texture = apply_effect(frame.data)
            new_frame = frame.clone_with_texture(processed_texture)
        """
        return VideoFrame(
            data=new_texture,
            timestamp=timestamp or self.timestamp,
            frame_number=self.frame_number,
            width=self.width,
            height=self.height,
            metadata=self.metadata.copy() if self.metadata else None
        )


@dataclass
class AudioBuffer:
    """
    Audio buffer message.

    IMPORTANT: data is a WebGPU buffer (wgpu.GPUBuffer).
    No NumPy arrays - GPU only!

    Attributes:
        data: WebGPU buffer (wgpu.GPUBuffer) - float32 PCM audio data
        timestamp: Absolute timestamp in seconds (from tick)
        sample_rate: Sample rate in Hz (e.g., 48000)
        channels: Number of channels (1=mono, 2=stereo)
        samples: Number of samples in the buffer
        metadata: Optional metadata dict (codec info, etc.)
    """
    data: Any  # wgpu.GPUBuffer (using Any to avoid import dependency)
    timestamp: float
    sample_rate: int
    channels: int
    samples: int  # Number of samples (needed since buffer is opaque)
    metadata: Optional[Dict[str, Any]] = None

    def __post_init__(self):
        """Validate audio data (WebGPU buffers only)."""
        data_type = type(self.data).__name__

        # WebGPU buffer validation
        if 'Buffer' in data_type or 'GPUBuffer' in data_type:
            # WebGPU buffer - trust parameters
            # Buffer is opaque GPU memory
            # Runtime validates size matches samples*channels*4 (float32)
            return

        # If we get here, data is not a WebGPU buffer
        raise TypeError(
            f"AudioBuffer.data must be a WebGPU buffer, got {data_type}. "
            f"streamlib is GPU-only - no NumPy support. "
            f"See docs on WebGPU-first architecture."
        )

    @property
    def duration(self) -> float:
        """Get duration in seconds."""
        return self.samples / self.sample_rate


@dataclass
class KeyEvent:
    """
    Keyboard event message.

    Attributes:
        key: Key code or character
        pressed: True if pressed, False if released
        timestamp: Event timestamp
        modifiers: Modifier keys (shift, ctrl, alt, etc.)
    """
    key: str
    pressed: bool
    timestamp: float
    modifiers: Optional[Dict[str, bool]] = None


@dataclass
class MouseEvent:
    """
    Mouse event message.

    Attributes:
        x: X coordinate
        y: Y coordinate
        button: Button number (0=left, 1=middle, 2=right, -1=move)
        pressed: True if pressed, False if released (None for move)
        timestamp: Event timestamp
    """
    x: int
    y: int
    button: int
    pressed: Optional[bool]
    timestamp: float


@dataclass
class DataMessage:
    """
    Generic data message.

    Used for arbitrary data that doesn't fit other message types.

    Attributes:
        data: Arbitrary data (any Python object)
        timestamp: Message timestamp
        data_type: Type hint (string, e.g., 'json', 'binary', 'custom')
        metadata: Optional metadata dict
    """
    data: Any
    timestamp: float
    data_type: str = 'unknown'
    metadata: Optional[Dict[str, Any]] = None
