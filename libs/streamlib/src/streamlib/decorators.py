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
                # Read ALL available buffers (not just latest!)
                # Audio chunks arrive faster than video frame rate
                # Using read_latest() would skip chunks and cause audio dropout
                buffers = self.inputs['audio'].read_all()

                for buffer in buffers:
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


def audio_source(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    device_name: Optional[str] = None,
    sample_rate: int = 48000,
    chunk_size: int = 512
) -> Callable:
    """
    Decorator that converts a simple audio configuration into a StreamHandler.

    The decorated function should have signature:
        def audio_func(gpu: GPUContext, device_name: Optional[str] = None) -> AudioBuffer

    The decorator automatically:
    - Creates audio capture on handler start
    - Outputs audio port
    - Handles chunk processing in real-time
    - Uploads audio chunks to GPU and emits to output
    - Low-latency streaming architecture (~10ms chunks @ 512 samples)

    Args:
        func: Function to decorate (provided automatically when used as @audio_source)
        handler_id: Optional handler ID (defaults to function name)
        device_name: Audio device name substring (None = default device)
        sample_rate: Sample rate in Hz (default 48000)
        chunk_size: Samples per chunk (default 512 = ~10.7ms @ 48kHz)

    Returns:
        StreamHandler subclass instance that wraps audio capture

    Example:
        @audio_source(device_name="MacBook", sample_rate=48000, chunk_size=512)
        def my_microphone(gpu: GPUContext, device_name: Optional[str] = None) -> AudioBuffer:
            '''
            Simple audio source - no code needed!
            Returns GPU audio buffers automatically.
            '''
            # This function body is optional - decorator handles everything
            # You can add custom logic here if needed
            pass

        # Use in pipeline (real-time streaming with GPU upload!)
        runtime = StreamRuntime(fps=30)
        runtime.add_stream(Stream(my_microphone))
        runtime.connect(my_microphone.outputs['audio'], reverb.inputs['audio'])
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class AudioSourceHandler(StreamHandler):
            """Auto-generated handler from @audio_source decorator."""

            def __init__(
                self,
                source_id: Optional[str] = None,
                audio_device_name: Optional[str] = None,
                audio_sample_rate: int = 48000,
                audio_chunk_size: int = 512
            ):
                super().__init__(handler_id=source_id or f.__name__)
                self.source_func = f
                self.audio_device_name = audio_device_name
                self.audio_sample_rate = audio_sample_rate
                self.audio_chunk_size = audio_chunk_size
                self.capture = None
                self._chunk_counter = 0
                self._lock = __import__('threading').Lock()

                # Create output port only (sources have no inputs)
                self.outputs['audio'] = AudioOutput('audio')

            async def on_start(self) -> None:
                """Create audio capture when handler starts."""
                # Import audio capture
                from .gpu.audio import AudioCapture

                # Create audio capture with callback
                self.capture = AudioCapture(
                    gpu_context=self._runtime.gpu_context,
                    sample_rate=self.audio_sample_rate,
                    chunk_size=self.audio_chunk_size,
                    device_name=self.audio_device_name,
                    process_callback=self._process_audio_chunk
                )

                # Start capture
                self.capture.start()

            def _process_audio_chunk(self, audio_chunk):
                """
                Callback for audio chunks (runs on audio thread).

                Uploads chunk to GPU and writes to output port.

                Args:
                    audio_chunk: numpy array (float32, mono)

                Returns:
                    numpy array (processed, same size)
                """
                try:
                    # Get current tick timestamp
                    # Note: Audio thread doesn't have tick, use monotonic time
                    import time
                    timestamp = time.perf_counter()

                    # Upload to GPU (creates AudioBuffer)
                    audio_buffer = AudioBuffer.create_from_numpy(
                        self._runtime.gpu_context,
                        audio_chunk,
                        timestamp=timestamp,
                        sample_rate=self.audio_sample_rate
                    )

                    # Write to output port (thread-safe)
                    self.outputs['audio'].write(audio_buffer)

                    # Track chunks
                    with self._lock:
                        self._chunk_counter += 1

                    # Return unmodified (passthrough)
                    return audio_chunk

                except Exception as e:
                    print(f"[{self.handler_id}] Error processing audio chunk: {e}")
                    import traceback
                    traceback.print_exc()
                    return audio_chunk  # Passthrough on error

            async def process(self, tick: TimedTick) -> None:
                """
                Process tick (audio capture runs on background thread).

                No action needed here - audio chunks are pushed via callback.
                """
                pass

            async def on_stop(self) -> None:
                """Stop audio capture when handler stops."""
                if self.capture:
                    self.capture.stop()

                # Print stats
                print(f"[{self.handler_id}] Captured {self._chunk_counter} audio chunks")

            def __repr__(self) -> str:
                return f"AudioSource('{self.handler_id}', device={self.audio_device_name}, " \
                       f"{self.audio_sample_rate}Hz, {self.audio_chunk_size} samples/chunk)"

        # Create and return handler instance
        return AudioSourceHandler(handler_id, device_name, sample_rate, chunk_size)

    # Handle both @audio_source and @audio_source() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


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
    title: str = "streamlib Display",
    show_fps: bool = False
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
        show_fps: If True, display FPS counter in window title

    Returns:
        StreamHandler subclass instance that wraps display sink

    Example:
        @display_sink(title="Camera Feed", show_fps=True)
        def my_display():
            '''
            Simple display sink - no code needed!
            Renders incoming frames automatically with FPS counter.
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
                display_title: str = "streamlib Display",
                display_show_fps: bool = False
            ):
                super().__init__(handler_id=sink_id or f.__name__)
                self.sink_func = f
                self.display_width = display_width
                self.display_height = display_height
                self.display_title = display_title
                self.display_show_fps = display_show_fps
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
                    title=self.display_title,
                    show_fps=self.display_show_fps
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
        return DisplaySinkHandler(handler_id, width, height, title, show_fps)

    # Handle both @display_sink and @display_sink() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


def audio_sink_file(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    output_path: str = "output.wav"
) -> Callable:
    """
    Decorator that converts audio stream to file output.

    The decorated function should have signature:
        def file_sink(gpu: GPUContext, output_path: str) -> None

    The decorator automatically:
    - Creates audio input port
    - Downloads audio chunks from GPU
    - Collects chunks in memory
    - Saves to WAV file on stop

    Args:
        func: Function to decorate (provided automatically when used as @audio_sink_file)
        handler_id: Optional handler ID (defaults to function name)
        output_path: Output file path (default: "output.wav")

    Returns:
        StreamHandler subclass instance that wraps file sink

    Example:
        @audio_sink_file(output_path="recording.wav")
        def my_file_sink():
            '''
            Simple file sink - no code needed!
            Saves incoming audio to WAV file automatically.
            '''
            # This function body is optional - decorator handles everything
            pass

        # Use in pipeline
        runtime = StreamRuntime(fps=30)
        runtime.add_stream(Stream(microphone))
        runtime.add_stream(Stream(my_file_sink))
        runtime.connect(microphone.outputs['audio'], my_file_sink.inputs['audio'])
        await runtime.start()
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class AudioFileSinkHandler(StreamHandler):
            """Auto-generated handler from @audio_sink_file decorator."""

            def __init__(
                self,
                sink_id: Optional[str] = None,
                file_output_path: str = "output.wav"
            ):
                super().__init__(handler_id=sink_id or f.__name__)
                self.sink_func = f
                self.output_path = file_output_path
                self.chunks = []
                self.sample_rate = None
                self._lock = __import__('threading').Lock()

                # Create input port only (sinks have no outputs)
                self.inputs['audio'] = AudioInput('audio')

            async def process(self, tick: TimedTick) -> None:
                """Download audio from GPU and collect chunks."""
                # Read ALL available audio buffers (not just latest!)
                # Audio chunks arrive at ~94 Hz but ticks are at 30 Hz
                # Using read_latest() would skip ~2/3 of chunks!
                audio_buffers = self.inputs['audio'].read_all()

                for audio_buffer in audio_buffers:
                    try:
                        # Store sample rate (for saving later)
                        if self.sample_rate is None:
                            self.sample_rate = audio_buffer.sample_rate

                        # Download audio from GPU buffer to CPU
                        # AudioBuffer.data is wgpu.GPUBuffer, need to read it
                        buffer_size = audio_buffer.samples * audio_buffer.channels * 4  # float32 = 4 bytes

                        # Read buffer from GPU (synchronous read)
                        buffer_data = self._runtime.gpu_context.device.queue.read_buffer(
                            audio_buffer.data,
                            size=buffer_size
                        )

                        # Convert to numpy array
                        import numpy as np
                        audio_chunk = np.frombuffer(buffer_data, dtype=np.float32).copy()

                        # Store chunk (thread-safe)
                        with self._lock:
                            self.chunks.append(audio_chunk)

                    except Exception as e:
                        print(f"[{self.handler_id}] Error collecting audio: {e}")
                        import traceback
                        traceback.print_exc()

            async def on_stop(self) -> None:
                """Save collected audio to file."""
                if not self.chunks:
                    print(f"[{self.handler_id}] No audio chunks to save")
                    return

                try:
                    print(f"\n💾 Saving {len(self.chunks)} audio chunks to {self.output_path}...")

                    # Concatenate all chunks
                    import numpy as np
                    audio_data = np.concatenate(self.chunks)

                    # Convert float32 [-1, 1] to int16 [-32768, 32767]
                    audio_int16 = (audio_data * 32767).astype(np.int16)

                    # Save as WAV file
                    from pathlib import Path
                    from scipy.io import wavfile
                    output_path = Path(self.output_path)
                    wavfile.write(output_path, self.sample_rate, audio_int16)

                    print(f"✅ Saved {len(audio_data):,} samples ({len(audio_data) / self.sample_rate:.2f}s)")
                    print(f"🎵 Play with: ffplay {self.output_path}")

                except Exception as e:
                    print(f"[{self.handler_id}] Error saving audio: {e}")
                    import traceback
                    traceback.print_exc()

            def __repr__(self) -> str:
                return f"AudioFileSink('{self.handler_id}', output={self.output_path})"

        # Create and return handler instance
        return AudioFileSinkHandler(handler_id, output_path)

    # Handle both @audio_sink_file and @audio_sink_file() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


def audio_sink_speaker(
    func: Optional[Callable] = None,
    *,
    handler_id: Optional[str] = None,
    device_name: Optional[str] = None,
    sample_rate: int = 48000,
    chunk_size: int = 512
) -> Callable:
    """
    Decorator that plays audio stream through speakers in real-time.

    The decorated function should have signature:
        def speaker_sink(gpu: GPUContext, device_name: Optional[str] = None) -> None

    The decorator automatically:
    - Creates audio output stream
    - Downloads audio chunks from GPU
    - Plays through speakers in real-time
    - Handles buffering and synchronization

    Args:
        func: Function to decorate (provided automatically when used as @audio_sink_speaker)
        handler_id: Optional handler ID (defaults to function name)
        device_name: Audio device name substring (None = default device)
        sample_rate: Sample rate in Hz (default 48000)
        chunk_size: Samples per chunk (default 512 = ~10.7ms @ 48kHz)

    Returns:
        StreamHandler subclass instance that wraps speaker sink

    Example:
        @audio_sink_speaker(device_name="Built-in Output", sample_rate=48000)
        def my_speakers():
            '''
            Simple speaker sink - no code needed!
            Plays incoming audio through speakers automatically.
            '''
            # This function body is optional - decorator handles everything
            pass

        # Use in pipeline
        runtime = StreamRuntime(fps=30)
        runtime.add_stream(Stream(microphone))
        runtime.add_stream(Stream(reverb_effect))
        runtime.add_stream(Stream(my_speakers))
        runtime.connect(microphone.outputs['audio'], reverb_effect.inputs['audio'])
        runtime.connect(reverb_effect.outputs['audio'], my_speakers.inputs['audio'])
        await runtime.start()
    """
    def decorator(f: Callable) -> StreamHandler:
        # Create handler class dynamically
        class AudioSpeakerSinkHandler(StreamHandler):
            """Auto-generated handler from @audio_sink_speaker decorator."""

            def __init__(
                self,
                sink_id: Optional[str] = None,
                speaker_device_name: Optional[str] = None,
                speaker_sample_rate: int = 48000,
                speaker_chunk_size: int = 512
            ):
                super().__init__(handler_id=sink_id or f.__name__)
                self.sink_func = f
                self.speaker_device_name = speaker_device_name
                self.speaker_sample_rate = speaker_sample_rate
                self.speaker_chunk_size = speaker_chunk_size
                self.stream = None
                self.device_id = None
                self._playback_buffer = []
                self._lock = __import__('threading').Lock()
                self._chunks_played = 0

                # Create input port only (sinks have no outputs)
                self.inputs['audio'] = AudioInput('audio')

            def _find_output_device(self) -> Optional[int]:
                """Find audio output device by name substring."""
                import sounddevice as sd

                if self.speaker_device_name is None:
                    # Use default output device
                    default_device = sd.default.device[1]  # [0] = input, [1] = output
                    devices = sd.query_devices()
                    if default_device is not None and devices[default_device]['max_output_channels'] > 0:
                        device_info = devices[default_device]
                        print(f"📢 Using default audio output: {device_info['name']} (id={default_device})")
                        print(f"   Channels: {device_info['max_output_channels']}, Sample Rate: {device_info['default_samplerate']:.0f}Hz")
                        return default_device, device_info
                    return None, None

                # Query all devices
                devices = sd.query_devices()

                # Find all matching output devices
                matches = []
                for idx, device in enumerate(devices):
                    if (device['max_output_channels'] > 0 and
                        self.speaker_device_name.lower() in device['name'].lower()):
                        matches.append((idx, device['name']))

                if not matches:
                    # Not found - show available devices
                    print(f"❌ Audio output device '{self.speaker_device_name}' not found.")
                    print("\n📢 Available Audio Output Devices:")
                    for idx, device in enumerate(devices):
                        if device["max_output_channels"] > 0:
                            default = " (default)" if idx == sd.default.device[1] else ""
                            print(
                                f"  [{idx}] {device['name']}{default} "
                                f"({device['max_output_channels']} channels, "
                                f"{device['default_samplerate']:.0f}Hz)"
                            )
                    raise RuntimeError(
                        f"Audio output device '{self.speaker_device_name}' not found. "
                        f"See available devices above."
                    )

                # If multiple matches, show warning
                if len(matches) > 1:
                    print(f"⚠️  Multiple output devices match '{self.speaker_device_name}':")
                    for idx, name in matches:
                        print(f"     [{idx}] {name}")
                    print(f"📢 Using first match: {matches[0][1]} (id={matches[0][0]})")
                else:
                    print(f"📢 Found audio output: {matches[0][1]} (id={matches[0][0]})")

                # Get device info for selected device
                device_id = matches[0][0]
                device_info = devices[device_id]
                print(f"   Channels: {device_info['max_output_channels']}, Sample Rate: {device_info['default_samplerate']:.0f}Hz")

                return device_id, device_info

            def _playback_callback(self, outdata, frames, time_info, status):
                """Callback for audio playback (runs on audio thread)."""
                if status:
                    print(f"⚠️  Playback status: {status}")

                import numpy as np

                # Get audio chunks from buffer
                with self._lock:
                    if self._playback_buffer:
                        # Get next chunk
                        chunk = self._playback_buffer.pop(0)

                        # Reshape to match output format (frames, channels)
                        if len(chunk) == frames:
                            outdata[:] = chunk.reshape(-1, 1)  # Mono
                        else:
                            # Size mismatch - pad or truncate
                            if len(chunk) < frames:
                                # Pad with zeros
                                padded = np.zeros(frames, dtype=np.float32)
                                padded[:len(chunk)] = chunk
                                outdata[:] = padded.reshape(-1, 1)
                            else:
                                # Truncate
                                outdata[:] = chunk[:frames].reshape(-1, 1)

                        self._chunks_played += 1
                    else:
                        # No data available - output silence
                        outdata.fill(0)

            async def on_start(self) -> None:
                """Create audio output stream when handler starts."""
                import sounddevice as sd

                # Find output device
                self.device_id, self.device_info = self._find_output_device()

                # Use device's native channel count
                device_channels = self.device_info['max_output_channels'] if self.device_info else 1

                # Create output stream
                self.stream = sd.OutputStream(
                    device=self.device_id,
                    channels=1,  # Mono output (convert in callback if needed)
                    samplerate=self.speaker_sample_rate,
                    blocksize=self.speaker_chunk_size,
                    callback=self._playback_callback,
                    dtype='float32'
                )

                # Start stream
                self.stream.start()

                print(f"🔊 Audio playback started")
                print(f"   Device: {self.device_info['name'] if self.device_info else 'default'}")
                print(f"   Sample Rate: {self.speaker_sample_rate}Hz, Chunk Size: {self.speaker_chunk_size} samples")

            async def process(self, tick: TimedTick) -> None:
                """Download audio from GPU and add to playback buffer."""
                # Read ALL available audio buffers (not just latest!)
                audio_buffers = self.inputs['audio'].read_all()

                for audio_buffer in audio_buffers:
                    try:
                        # Download audio from GPU buffer to CPU
                        buffer_size = audio_buffer.samples * audio_buffer.channels * 4  # float32 = 4 bytes

                        # Read buffer from GPU
                        buffer_data = self._runtime.gpu_context.device.queue.read_buffer(
                            audio_buffer.data,
                            size=buffer_size
                        )

                        # Convert to numpy array
                        import numpy as np
                        audio_chunk = np.frombuffer(buffer_data, dtype=np.float32).copy()

                        # Add to playback buffer (thread-safe)
                        with self._lock:
                            self._playback_buffer.append(audio_chunk)

                            # Prevent buffer from growing too large (drop old chunks)
                            if len(self._playback_buffer) > 10:
                                dropped = self._playback_buffer.pop(0)
                                print(f"⚠️  Dropped audio chunk (buffer overflow)")

                    except Exception as e:
                        print(f"[{self.handler_id}] Error processing audio for playback: {e}")
                        import traceback
                        traceback.print_exc()

            async def on_stop(self) -> None:
                """Stop audio playback when handler stops."""
                if self.stream:
                    self.stream.stop()
                    self.stream.close()
                    self.stream = None

                print(f"[{self.handler_id}] Played {self._chunks_played} audio chunks")

            def __repr__(self) -> str:
                return f"AudioSpeakerSink('{self.handler_id}', device={self.speaker_device_name})"

        # Create and return handler instance
        return AudioSpeakerSinkHandler(handler_id, device_name, sample_rate, chunk_size)

    # Handle both @audio_sink_speaker and @audio_sink_speaker() syntax
    if func is None:
        return decorator
    else:
        return decorator(func)


__all__ = [
    'video_effect',
    'audio_effect',
    'stream_processor',
    'audio_source',
    'camera_source',
    'display_sink',
    'audio_sink_file',
    'audio_sink_speaker',
]
