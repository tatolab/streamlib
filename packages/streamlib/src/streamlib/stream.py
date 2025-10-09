"""
Stream configuration wrapper.

Stream wraps a StreamHandler with execution configuration:
- Handler: The processing logic (inert until activated)
- Dispatcher: How to execute (asyncio, threadpool, gpu, processpool)
- Transport: Optional I/O configuration (for network/file handlers)

Example:
    handler = BlurFilter()
    stream = Stream(handler, dispatcher='threadpool')

    runtime = StreamRuntime()
    runtime.add_stream(stream)
"""

from typing import Optional, Dict, Union
from .handler import StreamHandler
from .dispatchers import Dispatcher, AsyncioDispatcher, ThreadPoolDispatcher


class Stream:
    """
    Configuration wrapper for StreamHandler.

    Wraps handler with dispatcher and optional transport config.
    StreamRuntime uses this to activate handlers with correct execution context.

    Example:
        # Simple stream with asyncio dispatcher
        stream = Stream(handler, dispatcher='asyncio')

        # Stream with thread pool for CPU-bound work
        stream = Stream(handler, dispatcher='threadpool')

        # Stream with custom dispatcher instance
        stream = Stream(handler, dispatcher=ThreadPoolDispatcher(max_workers=4))

        # Stream with transport config (for I/O handlers)
        stream = Stream(
            file_reader_handler,
            dispatcher='asyncio',
            transport={'path': '/path/to/video.mp4', 'loop': True}
        )
    """

    def __init__(
        self,
        handler: StreamHandler,
        dispatcher: Union[str, Dispatcher] = 'asyncio',
        transport: Optional[Dict] = None,
        **kwargs
    ):
        """
        Initialize stream configuration.

        Args:
            handler: StreamHandler instance (inert, not yet activated)
            dispatcher: Dispatcher type or instance. String options:
                - 'asyncio' - I/O-bound (default)
                - 'threadpool' - CPU-bound
                - 'gpu' - GPU-accelerated (stub)
                - 'processpool' - Heavy compute (stub)
                Or pass custom Dispatcher instance.
            transport: Optional transport configuration dict.
                Used by I/O handlers (file, network, camera).
                Internal processing handlers don't need this.
            **kwargs: Additional stream-specific config

        Example:
            stream = Stream(
                BlurFilter(),
                dispatcher='threadpool'  # CPU-bound processing
            )
        """
        if not isinstance(handler, StreamHandler):
            raise TypeError(f"handler must be StreamHandler, got {type(handler)}")

        self.handler = handler

        # Store dispatcher (string or instance)
        if isinstance(dispatcher, str):
            if dispatcher not in {'asyncio', 'threadpool', 'gpu', 'processpool'}:
                raise ValueError(
                    f"dispatcher must be 'asyncio', 'threadpool', 'gpu', or 'processpool', "
                    f"got '{dispatcher}'"
                )
        elif not isinstance(dispatcher, Dispatcher):
            raise TypeError(f"dispatcher must be str or Dispatcher, got {type(dispatcher)}")

        self.dispatcher = dispatcher
        self.transport = transport or {}
        self.config = kwargs

    def __repr__(self) -> str:
        dispatcher_str = self.dispatcher if isinstance(self.dispatcher, str) else type(self.dispatcher).__name__
        return f"Stream(handler={self.handler.handler_id}, dispatcher={dispatcher_str})"
