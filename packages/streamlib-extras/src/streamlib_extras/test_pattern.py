"""
GPU-native test pattern generator.

Generates test patterns directly on GPU using WebGPU compute shaders.
Zero CPU→GPU transfers - true GPU-first architecture.
"""

from typing import Optional, Literal
from pathlib import Path

from streamlib.handler import StreamHandler
from streamlib.ports import VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick
from streamlib.gpu import GPUContext, ComputeShader

import struct


PatternType = Literal['smpte_bars', 'gradient', 'solid', 'checkerboard']


class TestPatternHandler(StreamHandler):
    """
    GPU-native test pattern generator.

    Generates patterns directly on GPU using compute shaders.
    Output is GPU-native (WebGPU buffer), eliminating CPU→GPU transfers.

    Patterns:
    - smpte_bars: Classic 7-bar color test pattern
    - gradient: Horizontal black-to-white gradient
    - solid: Solid color fill
    - checkerboard: 8x8 checkerboard pattern

    Example:
        ```python
        pattern = TestPatternHandler(
            width=1920,
            height=1080,
            pattern='smpte_bars'
        )
        runtime.add_stream(Stream(pattern))  # Uses 'asyncio' by default
        ```

    GPU-first philosophy:
    - Pattern generated on GPU (compute shader)
    - Data never touches CPU
    - Output is GPU buffer (not numpy array)
    - Zero memory transfers
    """

    preferred_dispatcher = 'asyncio'  # GPU operations are non-blocking

    def __init__(
        self,
        width: int = 640,
        height: int = 480,
        pattern: PatternType = 'smpte_bars',
        color: Optional[tuple] = None,
        handler_id: str = None
    ):
        """
        Initialize GPU-native test pattern generator.

        Args:
            width: Frame width in pixels
            height: Frame height in pixels
            pattern: Pattern type ('smpte_bars', 'gradient', 'solid', 'checkerboard')
            color: RGB color for 'solid' pattern (0-255), defaults to white
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'test-pattern')

        self.width = width
        self.height = height
        self.pattern = pattern
        self.color = color or (255, 255, 255)

        # Output: GPU buffer (WebGPU)
        self.outputs['video'] = VideoOutput('video')  # GPU by default

        # Frame counter
        self._frame_number = 0

        # GPU resources (initialized in on_start)
        self._gpu_ctx: Optional[GPUContext] = None
        self._compute_shader: Optional[ComputeShader] = None
        self._output_buffer = None
        self._params_buffer = None

        # Pattern mode mapping
        self._pattern_modes = {
            'smpte_bars': 0,
            'gradient': 1,
            'solid': 2,
            'checkerboard': 3,
        }

    async def on_start(self) -> None:
        """Initialize GPU context and compute shader."""
        # Use runtime's shared WebGPU context (no fallback)
        self._gpu_ctx = self._runtime.gpu_context
        if not self._gpu_ctx:
            raise RuntimeError(
                f"[{self.handler_id}] GPU context not available from runtime. "
                f"Ensure runtime has enable_gpu=True"
            )

        # Load shader (colocated with handler in streamlib-extras)
        shader_path = Path(__file__).parent / 'shaders' / 'test_pattern.wgsl'
        shader_code = shader_path.read_text()

        # Create output buffer (RGBA packed as u32)
        buffer_size = self.width * self.height * 4  # 4 bytes per pixel (RGBA)
        self._output_buffer = self._gpu_ctx.create_buffer(
            size=buffer_size,
            usage=0x80 | 0x4,  # STORAGE | COPY_SRC
            label='test_pattern_output'
        )

        # Create params buffer (uniform)
        # struct PatternParams { width: u32, height: u32, mode: u32, color_r/g/b: u32 }
        params_data = struct.pack(
            'IIIIII',
            self.width,
            self.height,
            self._pattern_modes[self.pattern],
            self.color[0],
            self.color[1],
            self.color[2]
        )
        self._params_buffer = self._gpu_ctx.create_buffer(
            size=len(params_data),
            usage=0x40 | 0x8,  # UNIFORM | COPY_DST
            label='test_pattern_params'
        )
        self._gpu_ctx.queue.write_buffer(self._params_buffer, 0, params_data)

        # Create compute shader
        self._compute_shader = ComputeShader.from_wgsl(
            context=self._gpu_ctx,
            shader_code=shader_code,
            entry_point='main',
            bindings=[
                {'binding': 0, 'type': 'storage'},      # output_buffer
                {'binding': 1, 'type': 'uniform'},      # params
            ]
        )

        print(f"TestPatternHandler started: {self.width}x{self.height} @ {self.pattern} (GPU-native)")

    async def process(self, tick: TimedTick) -> None:
        """
        Generate test pattern on GPU.

        Dispatches compute shader to generate pattern directly on GPU.
        Output is GPU buffer - no CPU→GPU transfer needed!
        """
        if self._compute_shader is None:
            return

        # Dispatch compute shader (16x16 workgroup size)
        workgroup_count_x = (self.width + 15) // 16
        workgroup_count_y = (self.height + 15) // 16

        self._compute_shader.dispatch(
            workgroup_count=(workgroup_count_x, workgroup_count_y, 1),
            bindings={
                0: self._output_buffer,
                1: self._params_buffer,
            }
        )

        # Create VideoFrame with GPU buffer
        # Note: data is GPU buffer, not numpy array!
        frame = VideoFrame(
            data=self._output_buffer,  # WebGPU buffer
            timestamp=tick.timestamp,
            frame_number=self._frame_number,
            width=self.width,
            height=self.height,
            metadata={
                'pattern': self.pattern,
                'gpu': True,
                'backend': 'webgpu'
            }
        )

        # Write to output
        self.outputs['video'].write(frame)
        self._frame_number += 1

    async def on_stop(self) -> None:
        """Clean up GPU resources."""
        print(f"TestPatternHandler stopped: {self._frame_number} frames generated (GPU)")
