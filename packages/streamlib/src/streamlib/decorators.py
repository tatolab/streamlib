"""
High-level decorators for AI-friendly stream processing.

This module provides simple decorators that hide StreamHandler boilerplate,
making it easy for AI agents to generate correct stream processing code.

IMPORTANT: All video processing uses WebGPU textures. No NumPy, no PyTorch.
Data stays on GPU throughout the pipeline.

GPU Context Injection:
    The decorator automatically injects the GPU context as the 'gpu' parameter.
    Your effect function signature should be: (frame, gpu) -> VideoFrame

Example:
    from streamlib import video_effect, VideoFrame
    from streamlib.shaders import GRAYSCALE_SHADER

    @video_effect
    def my_grayscale(frame: VideoFrame, gpu: GPUContext) -> VideoFrame:
        '''GPU grayscale effect using WGSL compute shader.'''
        # GPU context is automatically injected!
        # frame.data is a WebGPU texture - stays on GPU!

        # Cache pipeline on handler instance (created once)
        handler = my_grayscale  # Decorator returns the handler
        if not hasattr(handler, 'pipeline'):
            handler.pipeline = gpu.create_compute_pipeline(GRAYSCALE_SHADER)

        # Create output texture and run shader (GPU → GPU, no CPU transfer)
        output = gpu.create_texture(frame.width, frame.height)
        gpu.run_compute(handler.pipeline, input=frame.data, output=output)

        return frame.clone_with_texture(output)

    # Use in pipeline
    runtime.add_stream(Stream(my_grayscale))
"""

from typing import Callable, Optional, Any, Dict
from functools import wraps
from .handler import StreamHandler
from .ports import VideoInput, VideoOutput, AudioInput, AudioOutput
from .messages import VideoFrame, AudioBuffer
from .clocks import TimedTick


def video_effect(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    pass_params: bool = False
) -> Callable:
    """
    Decorator that converts a simple video processing function into a StreamHandler.

    The decorated function should have signature:
        def effect(frame: VideoFrame, **kwargs) -> VideoFrame

    The decorator automatically:
    - Creates input/output ports (GPU-first by default)
    - Handles tick processing
    - Reads from input, calls function, writes to output
    - Preserves frame metadata and timing

    Args:
        func: Function to decorate (provided automatically when used as @video_effect)
        handler_id: Optional handler ID (defaults to function name)
        pass_params: If True, passes additional parameters to function from handler attributes

    Returns:
        StreamHandler subclass instance that wraps the function

    Example:
        @video_effect
        def blur_effect(frame: VideoFrame, gpu: GPUContext) -> VideoFrame:
            '''GPU blur using WGSL compute shader.'''
            # GPU context is automatically injected by the decorator!
            # frame.data is a WebGPU texture - stays on GPU!

            # Create pipeline once (cache on handler)
            handler = blur_effect  # The decorator returns the handler instance
            if not hasattr(handler, 'pipeline'):
                BLUR_SHADER = '''
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coord = vec2<i32>(gid.xy);
    let color = textureLoad(input_texture, coord, 0);
    textureStore(output_texture, coord, color);
}
'''
                handler.pipeline = gpu.create_compute_pipeline(BLUR_SHADER)

            # Create output texture and run shader (GPU → GPU)
            output = gpu.create_texture(frame.width, frame.height)
            gpu.run_compute(handler.pipeline, input=frame.data, output=output)

            return frame.clone_with_texture(output)

        # Use in pipeline
        runtime = StreamRuntime(fps=30)
        runtime.add_stream(Stream(blur_effect))
        runtime.connect(camera.outputs['video'], blur_effect.inputs['video'])
        runtime.connect(blur_effect.outputs['video'], display.inputs['video'])
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class VideoEffectHandler(StreamHandler):
            """Auto-generated handler from @video_effect decorator."""

            def __init__(self, effect_func: Callable, effect_id: Optional[str] = None, **kwargs):
                super().__init__(handler_id=effect_id or f.__name__)
                self.effect_func = effect_func
                self.effect_params = kwargs

                # Create GPU-first ports (runtime handles everything automatically)
                self.inputs['video'] = VideoInput('video')
                self.outputs['video'] = VideoOutput('video')

            async def process(self, tick: TimedTick) -> None:
                """Process video frame through effect function."""
                # Read latest frame (zero-copy)
                frame = self.inputs['video'].read_latest()
                if frame is None:
                    return

                # Call effect function with GPU context injection
                try:
                    # Inject GPU context from runtime
                    gpu_ctx = self._runtime.gpu_context

                    if pass_params:
                        result = self.effect_func(frame, gpu=gpu_ctx, **self.effect_params)
                    else:
                        result = self.effect_func(frame, gpu=gpu_ctx)

                    # Ensure result is VideoFrame
                    if not isinstance(result, VideoFrame):
                        raise TypeError(
                            f"Effect function {self.effect_func.__name__} must return VideoFrame, "
                            f"got {type(result)}"
                        )

                    # Write result (zero-copy)
                    self.outputs['video'].write(result)

                except Exception as e:
                    print(f"[{self.handler_id}] Error in effect function: {e}")
                    import traceback
                    traceback.print_exc()

            def __repr__(self) -> str:
                return f"VideoEffect('{self.handler_id}', func={self.effect_func.__name__})"

        # Create and return handler instance
        return VideoEffectHandler(f, handler_id)

    # Handle both @video_effect and @video_effect() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


def audio_effect(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    pass_params: bool = False
) -> Callable:
    """
    Decorator that converts a simple audio processing function into a StreamHandler.

    The decorated function should have signature:
        def effect(buffer: AudioBuffer, **kwargs) -> AudioBuffer

    The decorator automatically:
    - Creates input/output ports
    - Handles tick processing
    - Reads from input, calls function, writes to output
    - Preserves buffer metadata and timing

    Args:
        func: Function to decorate (provided automatically when used as @audio_effect)
        handler_id: Optional handler ID (defaults to function name)
        pass_params: If True, passes additional parameters to function from handler attributes

    Returns:
        StreamHandler subclass instance that wraps the function

    Example:
        @audio_effect
        def reverb_effect(buffer: AudioBuffer, gpu: GPUContext) -> AudioBuffer:
            '''GPU reverb using WGSL compute shader.'''
            # GPU context is automatically injected by the decorator!
            # buffer.data is a WebGPU buffer - stays on GPU!

            # Cache pipeline on handler instance
            handler = reverb_effect
            if not hasattr(handler, 'pipeline'):
                # Define your audio shader here
                handler.pipeline = gpu.create_compute_pipeline(REVERB_SHADER)

            # Run WGSL compute shader for audio DSP (GPU → GPU)
            output_buffer = gpu.create_buffer(buffer.samples * buffer.channels * 4)
            gpu.run_compute(
                handler.pipeline,
                input=buffer.data,
                output=output_buffer
            )

            return AudioBuffer(
                data=output_buffer,
                timestamp=buffer.timestamp,
                sample_rate=buffer.sample_rate,
                channels=buffer.channels,
                samples=buffer.samples
            )

        # Use in pipeline
        runtime = StreamRuntime(fps=30)
        runtime.add_stream(Stream(reverb_effect))
        runtime.connect(mic.outputs['audio'], reverb_effect.inputs['audio'])
        runtime.connect(reverb_effect.outputs['audio'], speaker.inputs['audio'])
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class AudioEffectHandler(StreamHandler):
            """Auto-generated handler from @audio_effect decorator."""

            def __init__(self, effect_func: Callable, effect_id: Optional[str] = None, **kwargs):
                super().__init__(handler_id=effect_id or f.__name__)
                self.effect_func = effect_func
                self.effect_params = kwargs

                # Create audio ports
                self.inputs['audio'] = AudioInput('audio')
                self.outputs['audio'] = AudioOutput('audio')

            async def process(self, tick: TimedTick) -> None:
                """Process audio buffer through effect function."""
                # Read latest buffer (zero-copy)
                buffer = self.inputs['audio'].read_latest()
                if buffer is None:
                    return

                # Call effect function with GPU context injection
                try:
                    # Inject GPU context from runtime
                    gpu_ctx = self._runtime.gpu_context

                    if pass_params:
                        result = self.effect_func(buffer, gpu=gpu_ctx, **self.effect_params)
                    else:
                        result = self.effect_func(buffer, gpu=gpu_ctx)

                    # Ensure result is AudioBuffer
                    if not isinstance(result, AudioBuffer):
                        raise TypeError(
                            f"Effect function {self.effect_func.__name__} must return AudioBuffer, "
                            f"got {type(result)}"
                        )

                    # Write result (zero-copy)
                    self.outputs['audio'].write(result)

                except Exception as e:
                    print(f"[{self.handler_id}] Error in effect function: {e}")
                    import traceback
                    traceback.print_exc()

            def __repr__(self) -> str:
                return f"AudioEffect('{self.handler_id}', func={self.effect_func.__name__})"

        # Create and return handler instance
        return AudioEffectHandler(f, handler_id)

    # Handle both @audio_effect and @audio_effect() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


def stream_processor(
    *,
    inputs: Dict[str, Any],
    outputs: Dict[str, Any],
    handler_id: Optional[str] = None
) -> Callable:
    """
    Decorator for custom multi-input/multi-output stream processors.

    More flexible than @video_effect or @audio_effect, but requires explicit port definitions.
    Use this for complex processors with multiple inputs/outputs.

    Args:
        inputs: Dictionary of input ports {name: InputPort}
        outputs: Dictionary of output ports {name: OutputPort}
        handler_id: Optional handler ID (defaults to function name)

    Returns:
        Decorator that wraps function in StreamHandler

    Example:
        @stream_processor(
            inputs={
                'video': VideoInput('video'),
                'overlay': VideoInput('overlay')
            },
            outputs={
                'video': VideoOutput('video')
            }
        )
        def compositor(tick: TimedTick, inputs: dict, outputs: dict) -> None:
            base = inputs['video'].read_latest()
            overlay = inputs['overlay'].read_latest()

            if base and overlay:
                composited = blend(base, overlay)
                outputs['video'].write(composited)

        # Use in pipeline
        runtime.add_stream(Stream(compositor))
        runtime.connect(camera.outputs['video'], compositor.inputs['video'])
        runtime.connect(logo.outputs['video'], compositor.inputs['overlay'])
        runtime.connect(compositor.outputs['video'], display.inputs['video'])
    """
    def decorator(func: Callable) -> StreamHandler:
        # Create handler class dynamically
        class CustomStreamHandler(StreamHandler):
            """Auto-generated handler from @stream_processor decorator."""

            def __init__(self, process_func: Callable, proc_id: Optional[str] = None):
                super().__init__(handler_id=proc_id or func.__name__)
                self.process_func = process_func

                # Set ports from decorator arguments
                self.inputs = inputs.copy()
                self.outputs = outputs.copy()

            async def process(self, tick: TimedTick) -> None:
                """Process tick through custom function."""
                try:
                    await self.process_func(tick, self.inputs, self.outputs)
                except Exception as e:
                    print(f"[{self.handler_id}] Error in processor function: {e}")
                    import traceback
                    traceback.print_exc()

            def __repr__(self) -> str:
                return f"StreamProcessor('{self.handler_id}', func={self.process_func.__name__})"

        # Create and return handler instance
        return CustomStreamHandler(func, handler_id)

    return decorator


def camera_source(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    device_id: Optional[str] = None
) -> Callable:
    """
    Decorator that converts a simple camera configuration into a StreamHandler.

    The decorated function should have signature:
        def camera_func(gpu: GPUContext, device_id: Optional[str] = None) -> VideoFrame

    The decorator automatically:
    - Creates camera capture on handler start
    - Outputs video port
    - Handles tick processing
    - Gets latest frame and emits to output
    - Zero-copy IOSurface → WebGPU texture on macOS

    Args:
        func: Function to decorate (provided automatically when used as @camera_source)
        handler_id: Optional handler ID (defaults to function name)
        device_id: Camera device ID (None = first available camera)

    Returns:
        StreamHandler subclass instance that wraps camera capture

    Example:
        @camera_source(device_id=None)  # First available camera
        def my_camera(gpu: GPUContext, device_id: Optional[str] = None) -> VideoFrame:
            '''
            Simple camera source - no code needed!
            Returns WebGPU texture frames automatically.
            '''
            # This function body is optional - decorator handles everything
            # You can add custom logic here if needed
            pass

        # Use in pipeline (zero-copy IOSurface → WebGPU on macOS!)
        runtime = StreamRuntime(fps=30, width=1920, height=1080)
        runtime.add_stream(Stream(my_camera))
        runtime.connect(my_camera.outputs['video'], blur.inputs['video'])
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class CameraSourceHandler(StreamHandler):
            """Auto-generated handler from @camera_source decorator."""

            def __init__(self, source_id: Optional[str] = None, cam_device_id: Optional[str] = None):
                super().__init__(handler_id=source_id or f.__name__)
                self.source_func = f
                self.camera_device_id = cam_device_id
                self.capture = None

                # Create output port only (sources have no inputs)
                self.outputs['video'] = VideoOutput('video')

            async def on_start(self) -> None:
                """Create camera capture when handler starts."""
                # Create camera capture (zero-copy IOSurface → WebGPU on macOS)
                self.capture = self._runtime.gpu_context.create_camera_capture(
                    device_id=self.camera_device_id
                )

            async def process(self, tick: TimedTick) -> None:
                """Get latest camera frame and emit to output."""
                if self.capture is None:
                    return

                try:
                    # Get latest frame (WebGPU texture, zero-copy from camera)
                    texture = self.capture.get_texture()

                    # Create VideoFrame
                    frame = VideoFrame(
                        data=texture,
                        timestamp=tick.timestamp,
                        frame_number=tick.frame_number,
                        width=self._runtime.width,
                        height=self._runtime.height
                    )

                    # Emit frame (user function is optional and not called by default)
                    # The decorator handles everything - users just write "pass"
                    self.outputs['video'].write(frame)

                except Exception as e:
                    print(f"[{self.handler_id}] Error in camera source: {e}")
                    import traceback
                    traceback.print_exc()

            async def on_stop(self) -> None:
                """Stop camera capture when handler stops."""
                if self.capture:
                    self.capture.stop()

            def __repr__(self) -> str:
                return f"CameraSource('{self.handler_id}', device_id={self.camera_device_id})"

        # Create and return handler instance
        return CameraSourceHandler(handler_id, device_id)

    # Handle both @camera_source and @camera_source() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


def display_sink(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    width: Optional[int] = None,
    height: Optional[int] = None,
    title: str = "streamlib Display"
) -> Callable:
    """
    Decorator that converts a simple display configuration into a StreamHandler.

    The decorated function should have signature:
        def display_func(frame: VideoFrame, gpu: GPUContext) -> None

    The decorator automatically:
    - Creates display window on handler start
    - Inputs video port
    - Handles tick processing
    - Reads latest frame and renders to display
    - Zero-copy rendering via WebGPU swapchain

    Args:
        func: Function to decorate (provided automatically when used as @display_sink)
        handler_id: Optional handler ID (defaults to function name)
        width: Window width (None = use runtime width)
        height: Window height (None = use runtime height)
        title: Window title

    Returns:
        StreamHandler subclass instance that wraps display sink

    Example:
        @display_sink(title="Camera Feed")
        def my_display():
            '''
            Simple display sink - no code needed!
            Renders incoming frames automatically.
            '''
            # This function body is optional - decorator handles everything
            # You can add custom logic here if needed
            pass

        # Use in pipeline (zero-copy rendering via swapchain!)
        runtime = StreamRuntime(fps=30, width=1920, height=1080)
        runtime.add_stream(Stream(camera))
        runtime.add_stream(Stream(my_display))
        runtime.connect(camera.outputs['video'], my_display.inputs['video'])
        await runtime.start()
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class DisplaySinkHandler(StreamHandler):
            """Auto-generated handler from @display_sink decorator."""

            def __init__(
                self,
                sink_id: Optional[str] = None,
                display_width: Optional[int] = None,
                display_height: Optional[int] = None,
                display_title: str = "streamlib Display"
            ):
                super().__init__(handler_id=sink_id or f.__name__)
                self.sink_func = f
                self.display_width = display_width
                self.display_height = display_height
                self.display_title = display_title
                self.display = None

                # Create input port only (sinks have no outputs)
                from .ports import VideoInput
                self.inputs['video'] = VideoInput('video')

            async def on_start(self) -> None:
                """Create display window when handler starts."""
                # Create display window (zero-copy rendering via swapchain)
                self.display = self._runtime.gpu_context.create_display(
                    width=self.display_width,
                    height=self.display_height,
                    title=self.display_title
                )

            async def process(self, tick: TimedTick) -> None:
                """Read latest frame and render to display."""
                if self.display is None:
                    return

                try:
                    # Read latest frame (zero-copy from ring buffer)
                    frame = self.inputs['video'].read_latest()

                    if frame is not None:
                        # Render to display (zero-copy to swapchain)
                        # User function is optional and not called by default
                        # The decorator handles everything - users just write "pass"
                        self.display.render(frame.data)

                except Exception as e:
                    print(f"[{self.handler_id}] Error in display sink: {e}")
                    import traceback
                    traceback.print_exc()

            async def on_stop(self) -> None:
                """Close display window when handler stops."""
                if self.display:
                    self.display.close()
                    self.display = None

            def __repr__(self) -> str:
                return f"DisplaySink('{self.handler_id}', title={self.display_title})"

        # Create and return handler instance
        return DisplaySinkHandler(handler_id, width, height, title)

    # Handle both @display_sink and @display_sink() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


__all__ = [
    'video_effect',
    'audio_effect',
    'stream_processor',
    'camera_source',
    'display_sink',
]
