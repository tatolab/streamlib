"""
Stream configuration wrapper.

Stream wraps a StreamHandler for direct use with StreamRuntime.
No dispatchers or execution context configuration - everything runs
on the shared WebGPU context.

Example:
    handler = BlurFilter()
    stream = Stream(handler)

    runtime = StreamRuntime()
    runtime.add_stream(stream)
"""

from typing import Optional, Dict
from .handler import StreamHandler


class Stream:
    """
    Configuration wrapper for StreamHandler.

    Wraps handler for use with StreamRuntime. All handlers run with
    the shared WebGPU context - no dispatcher configuration needed.

    Example:
        # Simple stream
        stream = Stream(handler)

        # Stream with transport config (for I/O handlers)
        stream = Stream(
            file_reader_handler,
            transport={'path': '/path/to/video.mp4', 'loop': True}
        )
    """

    def __init__(
        self,
        handler: StreamHandler,
        transport: Optional[Dict] = None,
        **kwargs
    ):
        """
        Initialize stream configuration.

        Args:
            handler: StreamHandler instance
            transport: Optional transport configuration dict.
                Used by I/O handlers (file, network, camera).
                Internal processing handlers don't need this.
            **kwargs: Additional stream-specific config

        Example:
            stream = Stream(BlurFilter())
            stream = Stream(camera_handler, transport={'device_id': 0})
        """
        if not isinstance(handler, StreamHandler):
            raise TypeError(
                f"handler must be StreamHandler instance, got {type(handler)}"
            )

        self.handler = handler
        self.transport = transport or {}
        self.config = kwargs

    def __repr__(self) -> str:
        return f"Stream(handler={self.handler.handler_id})"