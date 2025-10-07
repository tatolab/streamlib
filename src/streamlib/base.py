"""
Abstract base classes for the streaming library.

This module defines the core interfaces that all sources, sinks, layers, and
compositors must implement. The design follows the Unix philosophy of composable
primitives that can be chained together.
"""

from abc import ABC, abstractmethod
from typing import Any, AsyncIterator, Optional, Literal, Dict
import numpy as np
from numpy.typing import NDArray
from dataclasses import dataclass
import time


@dataclass
class TimestampedFrame:
    """
    A video frame with precise timing information.

    Attributes:
        frame: The frame data as a numpy array (height, width, channels)
        timestamp: Wall clock time when frame was captured (seconds since epoch)
        frame_number: Sequential frame number
        ptp_time: Optional PTP (IEEE 1588) synchronized time
        source_id: Optional identifier for the source that produced this frame
        metadata: Optional additional metadata
    """
    frame: NDArray[np.uint8]
    timestamp: float
    frame_number: int
    ptp_time: Optional[float] = None
    source_id: Optional[str] = None
    metadata: Optional[Dict[str, Any]] = None


class StreamSource(ABC):
    """
    Abstract base class for all stream sources.

    A source produces frames. Sources can be:
    - Hardware (webcam, screen capture, microphone)
    - Files (video files, image sequences)
    - Generated (test patterns, procedural content)
    - Network (remote sources over TCP/UDP/WebRTC)
    """

    def __init__(
        self,
        width: int = 1920,
        height: int = 1080,
        fps: int = 30,
        format: Literal['rgb24', 'bgr24', 'rgba', 'bgra', 'yuv420p'] = 'rgb24'
    ):
        self.width = width
        self.height = height
        self.fps = fps
        self.format = format
        self._frame_count = 0
        self._start_time = None

    @abstractmethod
    async def start(self) -> None:
        """Initialize the source and begin capturing/generating frames."""
        pass

    @abstractmethod
    async def stop(self) -> None:
        """Stop the source and release any resources."""
        pass

    @abstractmethod
    async def next_frame(self) -> TimestampedFrame:
        """
        Get the next frame from this source.

        Returns:
            TimestampedFrame with the next available frame

        Raises:
            EOFError: If the source has no more frames (e.g., end of video file)
        """
        pass

    async def frames(self) -> AsyncIterator[TimestampedFrame]:
        """
        Async iterator over all frames from this source.

        Yields:
            TimestampedFrame objects

        Example:
            async for frame in source.frames():
                await sink.write_frame(frame)
        """
        try:
            while True:
                yield await self.next_frame()
        except EOFError:
            return


class StreamSink(ABC):
    """
    Abstract base class for all stream sinks.

    A sink consumes frames. Sinks can be:
    - Files (video files, image sequences)
    - Network (send to remote consumers over TCP/UDP/WebRTC)
    - Display (show frames in a window)
    - HLS (HTTP Live Streaming)
    """

    def __init__(
        self,
        width: int = 1920,
        height: int = 1080,
        fps: int = 30,
        format: Literal['rgb24', 'bgr24', 'rgba', 'bgra', 'yuv420p'] = 'rgb24'
    ):
        self.width = width
        self.height = height
        self.fps = fps
        self.format = format

    @abstractmethod
    async def start(self) -> None:
        """Initialize the sink and prepare to receive frames."""
        pass

    @abstractmethod
    async def stop(self) -> None:
        """Stop the sink and finalize output (e.g., close file)."""
        pass

    @abstractmethod
    async def write_frame(self, frame: TimestampedFrame) -> None:
        """
        Write a frame to this sink.

        Args:
            frame: The timestamped frame to write
        """
        pass


class Layer(ABC):
    """
    Abstract base class for all compositable layers.

    A layer processes or generates visual content. Layers can be:
    - Video layers (pass-through or transformed video)
    - Drawing layers (execute Python drawing code with Skia)
    - ML layers (run machine learning models)

    Layers are composited together by a Compositor based on their z_index.
    """

    def __init__(
        self,
        name: str,
        z_index: int = 0,
        visible: bool = True,
        opacity: float = 1.0
    ):
        """
        Initialize a layer.

        Args:
            name: Unique identifier for this layer
            z_index: Layer ordering (higher values are on top)
            visible: Whether this layer should be rendered
            opacity: Layer opacity (0.0 = transparent, 1.0 = opaque)
        """
        self.name = name
        self.z_index = z_index
        self.visible = visible
        self.opacity = opacity

    @abstractmethod
    async def process_frame(
        self,
        input_frame: Optional[TimestampedFrame],
        width: int,
        height: int
    ) -> NDArray[np.uint8]:
        """
        Process/generate a frame for this layer.

        Args:
            input_frame: Optional input frame (can be None for generated layers)
            width: Target width for output
            height: Target height for output

        Returns:
            Numpy array of shape (height, width, 4) with RGBA data
        """
        pass

    def set_visible(self, visible: bool) -> None:
        """Show or hide this layer."""
        self.visible = visible

    def set_opacity(self, opacity: float) -> None:
        """Set layer opacity (0.0 to 1.0)."""
        self.opacity = max(0.0, min(1.0, opacity))

    def set_z_index(self, z_index: int) -> None:
        """Change layer ordering."""
        self.z_index = z_index


class Compositor(ABC):
    """
    Abstract base class for compositors.

    A compositor combines multiple layers into a single output frame.
    It manages layer ordering, alpha blending, and zero-copy numpy operations.
    """

    def __init__(self, width: int = 1920, height: int = 1080):
        """
        Initialize compositor.

        Args:
            width: Output width
            height: Output height
        """
        self.width = width
        self.height = height
        self.layers: Dict[str, Layer] = {}

    def add_layer(self, layer: Layer) -> None:
        """
        Add a layer to the compositor.

        Args:
            layer: Layer to add
        """
        self.layers[layer.name] = layer

    def remove_layer(self, name: str) -> None:
        """
        Remove a layer by name.

        Args:
            name: Name of layer to remove
        """
        if name in self.layers:
            del self.layers[name]

    def get_layer(self, name: str) -> Optional[Layer]:
        """
        Get a layer by name.

        Args:
            name: Name of layer to retrieve

        Returns:
            Layer if found, None otherwise
        """
        return self.layers.get(name)

    @abstractmethod
    async def composite(
        self,
        input_frame: Optional[TimestampedFrame] = None
    ) -> TimestampedFrame:
        """
        Composite all layers into a single output frame.

        Args:
            input_frame: Optional input frame to pass to layers

        Returns:
            Composited frame with all layers blended
        """
        pass

    def _sort_layers(self) -> list[Layer]:
        """
        Get layers sorted by z_index (lowest to highest).

        Returns:
            Sorted list of layers
        """
        return sorted(
            [layer for layer in self.layers.values() if layer.visible],
            key=lambda l: l.z_index
        )
