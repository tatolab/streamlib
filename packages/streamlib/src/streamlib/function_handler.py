"""
Function-based stream handler wrapper.

Allows functions to be used as stream handlers via decorator or direct wrapping.
"""

from typing import Callable, Dict
from .handler import StreamHandler
from .clocks import TimedTick


class FunctionHandler(StreamHandler):
    """
    Wrapper that converts a function into a StreamHandler.

    This allows simple functions to be used as handlers without
    creating a full class. Useful for quick prototypes and AI-generated code.

    Example:
        @stream_handler(
            inputs={'video': VideoInput('video')},
            outputs={'video': VideoOutput('video')},
            dispatcher='threadpool'
        )
        async def my_blur(tick, inputs, outputs):
            frame = inputs['video'].read_latest()
            if frame:
                blurred = cv2.GaussianBlur(frame.data, (5, 5), 0)
                outputs['video'].write(VideoFrame(blurred, tick.timestamp))

        runtime.add_stream(Stream(my_blur))
    """

    def __init__(
        self,
        process_func: Callable,
        inputs: Dict,
        outputs: Dict,
        handler_id: str = None
    ):
        """
        Initialize function handler.

        Args:
            process_func: Async function with signature (tick, inputs, outputs) -> None
            inputs: Dictionary of input ports
            outputs: Dictionary of output ports
            handler_id: Optional handler ID (defaults to function name)
        """
        super().__init__(handler_id or process_func.__name__)

        self.process_func = process_func
        self.inputs = inputs
        self.outputs = outputs

    async def process(self, tick: TimedTick) -> None:
        """
        Process tick by calling wrapped function.

        Args:
            tick: TimedTick from runtime clock
        """
        await self.process_func(tick, self.inputs, self.outputs)


def stream_handler(
    inputs: Dict = None,
    outputs: Dict = None,
    dispatcher: str = 'asyncio',
    handler_id: str = None
):
    """
    Decorator to convert a function into a stream handler.

    Attaches metadata to the function that Stream() can use to
    automatically create a FunctionHandler wrapper.

    Args:
        inputs: Dictionary of input ports {name: InputPort}
        outputs: Dictionary of output ports {name: OutputPort}
        dispatcher: Preferred dispatcher type ('asyncio', 'threadpool', 'gpu', 'processpool')
        handler_id: Optional handler identifier

    Example:
        from streamlib import stream_handler, VideoInput, VideoOutput
        from streamlib.messages import VideoFrame
        import cv2

        @stream_handler(
            inputs={'video': VideoInput('video', capabilities=['cpu'])},
            outputs={'video': VideoOutput('video', capabilities=['cpu'])},
            dispatcher='threadpool'
        )
        async def blur_filter(tick, inputs, outputs):
            '''AI-generated blur filter.'''
            frame = inputs['video'].read_latest()
            if frame:
                blurred = cv2.GaussianBlur(frame.data, (5, 5), 0)
                outputs['video'].write(VideoFrame(
                    data=blurred,
                    timestamp=tick.timestamp,
                    frame_number=tick.frame_number,
                    width=frame.width,
                    height=frame.height
                ))

        # Use directly with Stream
        runtime.add_stream(Stream(blur_filter))  # Uses decorator metadata
    """
    def decorator(func: Callable) -> Callable:
        # Attach metadata to function
        func._stream_metadata = {
            'inputs': inputs or {},
            'outputs': outputs or {},
            'dispatcher': dispatcher,
            'handler_id': handler_id
        }
        return func

    return decorator
