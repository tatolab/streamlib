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

from typing import Optional, Dict, Union, Callable
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
        handler: Union[StreamHandler, Callable],
        dispatcher: Optional[Union[str, Dispatcher]] = None,
        transport: Optional[Dict] = None,
        inputs: Optional[Dict] = None,
        outputs: Optional[Dict] = None,
        **kwargs
    ):
        """
        Initialize stream configuration.

        Args:
            handler: StreamHandler instance or callable function (decorated or plain).
            dispatcher: Optional dispatcher type or instance. String options:
                - 'asyncio' - I/O-bound (default)
                - 'threadpool' - CPU-bound
                - 'gpu' - GPU-accelerated (stub)
                - 'processpool' - Heavy compute (stub)
                Or pass custom Dispatcher instance.
                If None, uses handler.preferred_dispatcher (for StreamHandler)
                or metadata from decorator (for functions).
            transport: Optional transport configuration dict.
                Used by I/O handlers (file, network, camera).
                Internal processing handlers don't need this.
            inputs: Port configuration for function handlers.
            outputs: Port configuration for function handlers.
            **kwargs: Additional stream-specific config

        Example (class-based handler):
            stream = Stream(
                BlurFilter(),
                dispatcher='threadpool'  # Explicit override
            )

            # Or use handler's preferred dispatcher
            stream = Stream(BlurFilter())  # Uses BlurFilter.preferred_dispatcher

        Example (function handler):
            @stream_handler(
                inputs={'video': VideoInput('video')},
                outputs={'video': VideoOutput('video')},
                dispatcher='threadpool'
            )
            async def my_blur(tick, inputs, outputs):
                # ... processing
                pass

            stream = Stream(my_blur)  # Uses decorator metadata
        """
        # Case 1: StreamHandler (class-based)
        if isinstance(handler, StreamHandler):
            self.handler = handler

            # Use explicit dispatcher, or fall back to handler's preference, or default to 'asyncio'
            self.dispatcher = (
                dispatcher or
                getattr(handler, 'preferred_dispatcher', 'asyncio')
            )

        # Case 2: Decorated function with metadata
        elif callable(handler) and hasattr(handler, '_stream_metadata'):
            from .function_handler import FunctionHandler

            meta = handler._stream_metadata
            self.handler = FunctionHandler(
                process_func=handler,
                inputs=meta.get('inputs', {}),
                outputs=meta.get('outputs', {}),
                handler_id=meta.get('handler_id') or handler.__name__
            )
            # Use explicit dispatcher, or decorator's dispatcher, or default
            self.dispatcher = dispatcher or meta.get('dispatcher', 'asyncio')

        # Case 3: Plain function with explicit ports
        elif callable(handler):
            if inputs is None or outputs is None:
                raise ValueError(
                    "Plain functions require explicit 'inputs' and 'outputs' parameters. "
                    "Use @stream_handler decorator or pass inputs/outputs to Stream()."
                )

            from .function_handler import FunctionHandler

            self.handler = FunctionHandler(
                process_func=handler,
                inputs=inputs,
                outputs=outputs,
                handler_id=handler.__name__
            )
            self.dispatcher = dispatcher or 'asyncio'

        else:
            raise TypeError(
                f"handler must be StreamHandler or callable function, got {type(handler)}"
            )

        # Validate dispatcher
        if isinstance(self.dispatcher, str):
            if self.dispatcher not in {'asyncio', 'threadpool', 'gpu', 'processpool'}:
                raise ValueError(
                    f"dispatcher must be 'asyncio', 'threadpool', 'gpu', or 'processpool', "
                    f"got '{self.dispatcher}'"
                )
        elif not isinstance(self.dispatcher, Dispatcher):
            raise TypeError(
                f"dispatcher must be str or Dispatcher, got {type(self.dispatcher)}"
            )

        self.transport = transport or {}
        self.config = kwargs

    def __repr__(self) -> str:
        dispatcher_str = self.dispatcher if isinstance(self.dispatcher, str) else type(self.dispatcher).__name__
        return f"Stream(handler={self.handler.handler_id}, dispatcher={dispatcher_str})"
