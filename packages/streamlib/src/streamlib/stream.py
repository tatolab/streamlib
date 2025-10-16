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

from typing import Optional, Dict, Union, Callable
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
        handler: Union[StreamHandler, Callable],
        transport: Optional[Dict] = None,
        inputs: Optional[Dict] = None,
        outputs: Optional[Dict] = None,
        **kwargs
    ):
        """
        Initialize stream configuration.

        Args:
            handler: StreamHandler instance or callable function (decorated or plain).
            transport: Optional transport configuration dict.
                Used by I/O handlers (file, network, camera).
                Internal processing handlers don't need this.
            inputs: Port configuration for function handlers.
            outputs: Port configuration for function handlers.
            **kwargs: Additional stream-specific config

        Example (class-based handler):
            stream = Stream(BlurFilter())

        Example (function handler):
            @stream_handler(
                inputs={'video': VideoInput('video')},
                outputs={'video': VideoOutput('video')}
            )
            async def my_blur(tick, inputs, outputs):
                # ... processing
                pass

            stream = Stream(my_blur)
        """
        # Case 1: StreamHandler (class-based)
        if isinstance(handler, StreamHandler):
            self.handler = handler

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

        else:
            raise TypeError(
                f"handler must be StreamHandler or callable function, got {type(handler)}"
            )

        self.transport = transport or {}
        self.config = kwargs

    def __repr__(self) -> str:
        return f"Stream(handler={self.handler.handler_id})"