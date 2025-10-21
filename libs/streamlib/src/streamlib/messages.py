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

    @classmethod
    def create_from_numpy(
        cls,
        gpu_ctx: 'GPUContext',
        audio_data: Any,  # np.ndarray
        timestamp: float,
        sample_rate: int = 48000,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'AudioBuffer':
        """
        Create an AudioBuffer from a NumPy array (uploads to GPU).

        Args:
            gpu_ctx: GPU context for buffer creation
            audio_data: NumPy array (float32, shape: (samples,) for mono or (samples, channels))
            timestamp: Audio timestamp
            sample_rate: Sample rate in Hz (default 48000)
            metadata: Optional metadata

        Returns:
            AudioBuffer with data on GPU

        Example:
            import numpy as np
            audio = np.sin(2 * np.pi * 440 * np.linspace(0, 1, 48000)).astype(np.float32)
            buffer = AudioBuffer.create_from_numpy(gpu_ctx, audio, tick.timestamp)
        """
        import numpy as np

        # Validate input
        if not isinstance(audio_data, np.ndarray):
            raise TypeError(f"audio_data must be numpy array, got {type(audio_data)}")
        if audio_data.dtype != np.float32:
            raise TypeError(f"audio_data must be float32, got {audio_data.dtype}")

        # Determine channels and samples
        if audio_data.ndim == 1:
            channels = 1
            samples = len(audio_data)
        elif audio_data.ndim == 2:
            samples, channels = audio_data.shape
        else:
            raise ValueError(f"audio_data must be 1D (mono) or 2D (multichannel), got {audio_data.ndim}D")

        # Import wgpu for BufferUsage
        import wgpu

        # Create GPU buffer
        buffer_size = samples * channels * 4  # float32 = 4 bytes
        gpu_buffer = gpu_ctx.device.create_buffer(
            size=buffer_size,
            usage=wgpu.BufferUsage.STORAGE | wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.COPY_SRC
        )

        # Upload to GPU
        gpu_ctx.device.queue.write_buffer(gpu_buffer, 0, audio_data.tobytes())

        return cls(
            data=gpu_buffer,
            timestamp=timestamp,
            sample_rate=sample_rate,
            channels=channels,
            samples=samples,
            metadata=metadata
        )

    @classmethod
    def create_silence(
        cls,
        gpu_ctx: 'GPUContext',
        samples: int,
        timestamp: float,
        sample_rate: int = 48000,
        channels: int = 1,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'AudioBuffer':
        """
        Create a silent AudioBuffer (all zeros).

        Args:
            gpu_ctx: GPU context for buffer creation
            samples: Number of samples
            timestamp: Audio timestamp
            sample_rate: Sample rate in Hz (default 48000)
            channels: Number of channels (default 1)
            metadata: Optional metadata

        Returns:
            AudioBuffer with silent audio on GPU

        Example:
            # Create 1 second of silence
            buffer = AudioBuffer.create_silence(gpu_ctx, 48000, tick.timestamp)
        """
        import numpy as np

        silence = np.zeros(samples * channels, dtype=np.float32)
        return cls.create_from_numpy(
            gpu_ctx, silence, timestamp, sample_rate, metadata
        )

    @classmethod
    def create_from_buffer(
        cls,
        buffer: Any,  # wgpu.GPUBuffer
        timestamp: float,
        sample_rate: int,
        channels: int,
        samples: int,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'AudioBuffer':
        """
        Create an AudioBuffer from an existing GPU buffer.

        Args:
            buffer: WebGPU buffer (wgpu.GPUBuffer)
            timestamp: Audio timestamp
            sample_rate: Sample rate in Hz
            channels: Number of channels
            samples: Number of samples
            metadata: Optional metadata

        Returns:
            AudioBuffer wrapping the buffer

        Example:
            buffer = AudioBuffer.create_from_buffer(
                my_gpu_buffer, tick.timestamp, 48000, 1, 512
            )
        """
        return cls(
            data=buffer,
            timestamp=timestamp,
            sample_rate=sample_rate,
            channels=channels,
            samples=samples,
            metadata=metadata
        )

    def clone_with_buffer(
        self,
        new_buffer: Any,  # wgpu.GPUBuffer
        timestamp: Optional[float] = None
    ) -> 'AudioBuffer':
        """
        Create a new AudioBuffer with a different buffer but same metadata.

        Useful for effects that modify audio data but preserve timing.

        Args:
            new_buffer: New GPU buffer
            timestamp: Optional new timestamp (uses original if None)

        Returns:
            New AudioBuffer with replaced buffer

        Example:
            # Apply effect and create new buffer
            processed_buffer = apply_effect(buffer.data)
            new_buffer = buffer.clone_with_buffer(processed_buffer)
        """
        return AudioBuffer(
            data=new_buffer,
            timestamp=timestamp or self.timestamp,
            sample_rate=self.sample_rate,
            channels=self.channels,
            samples=self.samples,
            metadata=self.metadata.copy() if self.metadata else None
        )


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
