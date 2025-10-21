"""
GPU context for managing WebGPU device and queue.

This module provides a high-level context for GPU operations,
abstracting the underlying WebGPU backend.
"""

from typing import Optional, Dict, Any
from .backends.webgpu import WebGPUBackend
from .utils import GPUUtils
from .ml import MLRuntime

try:
    import wgpu
    HAS_WGPU = True
except ImportError:
    HAS_WGPU = False


class GPUContext:
    """
    GPU context for a StreamRuntime.

    Manages WebGPU device, queue, and provides high-level GPU operations.
    Each runtime should have its own GPU context (not global).

    Example:
        # Create context
        gpu_ctx = await GPUContext.create()

        # Get device info
        print(f"Using {gpu_ctx.backend_name} on {gpu_ctx.device_name}")

        # Create buffer
        buffer = gpu_ctx.create_buffer(size=1920*1080*3)

        # Use utility functions
        texture = gpu_ctx.utils.create_test_pattern(640, 480, 'smpte_bars')

        # Run ML model
        model = gpu_ctx.ml.load_model("yolov8n.onnx")
        results = gpu_ctx.ml.run(model, frame.data)
    """

    def __init__(self, backend: WebGPUBackend):
        """
        Initialize GPU context (use create() instead).

        Args:
            backend: WebGPU backend instance
        """
        self.backend = backend
        self._memory_pool: Dict[tuple, list] = {}  # (size, usage) -> [buffer, ...]

        # Attach utility modules
        self.utils = GPUUtils(self)         # Texture/buffer utilities
        self.ml = MLRuntime(self)           # ML model inference

    @classmethod
    async def create(
        cls,
        power_preference: str = 'high-performance'
    ) -> 'GPUContext':
        """
        Create GPU context (async).

        Args:
            power_preference: 'high-performance' or 'low-power'

        Returns:
            GPUContext instance

        Raises:
            RuntimeError: If WebGPU not available

        Example:
            gpu_ctx = await GPUContext.create()
        """
        if not HAS_WGPU:
            raise RuntimeError(
                "WebGPU not available. Install with: pip install wgpu"
            )

        # Create WebGPU backend
        backend = await WebGPUBackend.create(power_preference=power_preference)

        return cls(backend=backend)

    @property
    def device(self) -> 'wgpu.GPUDevice':
        """Get WebGPU device."""
        return self.backend.device

    @property
    def queue(self) -> 'wgpu.GPUQueue':
        """Get WebGPU command queue."""
        return self.backend.queue

    @property
    def adapter(self) -> 'wgpu.GPUAdapter':
        """Get WebGPU adapter."""
        return self.backend.adapter

    @property
    def backend_name(self) -> str:
        """
        Get backend name.

        Returns:
            'Metal' (macOS), 'D3D12' (Windows), or 'Vulkan' (Linux)
        """
        return self.backend.backend_name

    @property
    def device_name(self) -> str:
        """
        Get device name.

        Returns:
            GPU device name (e.g., "Apple M1 Pro", "NVIDIA RTX 4090")
        """
        info = self.backend.adapter_info
        return info.get('description', 'Unknown GPU')

    @property
    def limits(self) -> Dict[str, int]:
        """
        Get device limits.

        Returns:
            Dictionary with device limits
        """
        return self.backend.limits

    def create_buffer(
        self,
        size: int,
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'wgpu.GPUBuffer':
        """
        Create GPU buffer.

        Args:
            size: Buffer size in bytes
            usage: Buffer usage flags (default: STORAGE | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            WebGPU buffer

        Example:
            buffer = gpu_ctx.create_buffer(size=1920*1080*3)
        """
        if usage is None:
            usage = (
                wgpu.BufferUsage.STORAGE |
                wgpu.BufferUsage.COPY_DST |
                wgpu.BufferUsage.COPY_SRC
            )

        return self.backend.create_buffer(size=size, usage=usage, label=label)

    def create_texture(
        self,
        width: int,
        height: int,
        format: str = 'rgba8unorm',
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'wgpu.GPUTexture':
        """
        Create GPU texture.

        Args:
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (default: 'rgba8unorm')
            usage: Texture usage flags (default: TEXTURE_BINDING | COPY_DST | COPY_SRC | STORAGE_BINDING)
            label: Optional debug label

        Returns:
            WebGPU texture

        Example:
            texture = gpu_ctx.create_texture(width=1920, height=1080)
        """
        # Default usage includes STORAGE_BINDING for compute shaders
        if usage is None:
            usage = (
                wgpu.TextureUsage.TEXTURE_BINDING |
                wgpu.TextureUsage.COPY_DST |
                wgpu.TextureUsage.COPY_SRC |
                wgpu.TextureUsage.STORAGE_BINDING
            )

        return self.backend.create_texture(
            width=width,
            height=height,
            format=format,
            usage=usage,
            label=label
        )

    def allocate_buffer(self, size: int, usage: int) -> 'wgpu.GPUBuffer':
        """
        Allocate buffer from memory pool (reuse if available).

        Args:
            size: Buffer size in bytes
            usage: Buffer usage flags

        Returns:
            WebGPU buffer (reused or newly allocated)
        """
        key = (size, usage)

        # Try to reuse from pool
        if key in self._memory_pool and len(self._memory_pool[key]) > 0:
            return self._memory_pool[key].pop()

        # Allocate new buffer
        return self.create_buffer(size=size, usage=usage)

    def release_buffer(self, buffer: 'wgpu.GPUBuffer', size: int, usage: int) -> None:
        """
        Return buffer to memory pool for reuse.

        Args:
            buffer: Buffer to release
            size: Buffer size in bytes
            usage: Buffer usage flags
        """
        key = (size, usage)

        if key not in self._memory_pool:
            self._memory_pool[key] = []

        # Add to pool (limit pool size to avoid memory bloat)
        if len(self._memory_pool[key]) < 10:
            self._memory_pool[key].append(buffer)

    def clear_memory_pool(self) -> None:
        """Clear all buffers from memory pool."""
        self._memory_pool.clear()

    def create_compute_pipeline(
        self,
        wgsl_code: str,
        entry_point: str = 'main'
    ) -> 'wgpu.GPUComputePipeline':
        """
        Create compute pipeline from WGSL shader code.

        Simplified API for texture-based compute shaders.
        Assumes shader has bindings:
          @group(0) @binding(0) var input_texture: texture_2d<f32>;
          @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

        Args:
            wgsl_code: WGSL shader source code
            entry_point: Shader entry point function (default: 'main')

        Returns:
            Compute pipeline object with bind_group_layout attached

        Example:
            BLUR_SHADER = '''
            @group(0) @binding(0) var input_texture: texture_2d<f32>;
            @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
                let color = textureLoad(input_texture, vec2<i32>(gid.xy), 0);
                textureStore(output_texture, vec2<i32>(gid.xy), color);
            }
            '''

            pipeline = gpu_ctx.create_compute_pipeline(BLUR_SHADER)
        """
        # Create shader module
        shader_module = self.device.create_shader_module(code=wgsl_code)

        # Create bind group layout for texture bindings
        # Binding 0: Input texture (sampled)
        # Binding 1: Output texture (storage write)
        bind_group_layout = self.device.create_bind_group_layout(
            entries=[
                {
                    "binding": 0,
                    "visibility": wgpu.ShaderStage.COMPUTE,
                    "texture": {
                        "sample_type": wgpu.TextureSampleType.float,
                        "view_dimension": wgpu.TextureViewDimension.d2,
                    }
                },
                {
                    "binding": 1,
                    "visibility": wgpu.ShaderStage.COMPUTE,
                    "storage_texture": {
                        "access": wgpu.StorageTextureAccess.write_only,
                        "format": wgpu.TextureFormat.rgba8unorm,
                        "view_dimension": wgpu.TextureViewDimension.d2,
                    }
                }
            ]
        )

        # Create pipeline layout
        pipeline_layout = self.device.create_pipeline_layout(
            bind_group_layouts=[bind_group_layout]
        )

        # Create compute pipeline
        pipeline = self.device.create_compute_pipeline(
            layout=pipeline_layout,
            compute={
                "module": shader_module,
                "entry_point": entry_point,
            }
        )

        # Attach bind group layout to pipeline for run_compute()
        pipeline._bind_group_layout = bind_group_layout

        return pipeline

    def run_compute(
        self,
        pipeline: 'wgpu.GPUComputePipeline',
        input: 'wgpu.GPUTexture',
        output: 'wgpu.GPUTexture',
        workgroup_size: tuple = (8, 8, 1)
    ) -> None:
        """
        Run compute shader on textures.

        Args:
            pipeline: Compute pipeline from create_compute_pipeline()
            input: Input texture (binding 0)
            output: Output texture (binding 1)
            workgroup_size: Shader workgroup size (default: 8x8x1)

        Example:
            pipeline = gpu_ctx.create_compute_pipeline(BLUR_SHADER)
            output = gpu_ctx.create_texture(1920, 1080)
            gpu_ctx.run_compute(pipeline, input=frame.data, output=output)
        """
        # Calculate workgroup count
        # For 1920x1080 with workgroup_size=(8,8,1): (240, 135, 1)
        width = output.size[0]
        height = output.size[1]
        workgroup_count_x = (width + workgroup_size[0] - 1) // workgroup_size[0]
        workgroup_count_y = (height + workgroup_size[1] - 1) // workgroup_size[1]
        workgroup_count_z = workgroup_size[2]

        # Create texture views
        input_view = input.create_view()
        output_view = output.create_view()

        # Create bind group
        bind_group = self.device.create_bind_group(
            layout=pipeline._bind_group_layout,
            entries=[
                {
                    "binding": 0,
                    "resource": input_view
                },
                {
                    "binding": 1,
                    "resource": output_view
                }
            ]
        )

        # Create command encoder
        encoder = self.device.create_command_encoder()

        # Compute pass
        compute_pass = encoder.begin_compute_pass()
        compute_pass.set_pipeline(pipeline)
        compute_pass.set_bind_group(0, bind_group)
        compute_pass.dispatch_workgroups(
            workgroup_count_x,
            workgroup_count_y,
            workgroup_count_z
        )
        compute_pass.end()

        # Submit
        self.queue.submit([encoder.finish()])

    def get_output_texture(
        self,
        width: int,
        height: int,
        format: str = 'rgba8unorm'
    ) -> 'wgpu.GPUTexture':
        """
        Get or create output texture for compute shader results.

        This is a convenience method for decorators. In the future,
        this could use texture pooling to reuse textures.

        Args:
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (default: 'rgba8unorm')

        Returns:
            WebGPU texture for shader output

        Example:
            output = gpu_ctx.get_output_texture(1920, 1080)
            gpu_ctx.run_compute(shader, input=frame.data, output=output)
        """
        # For now, just create a new texture
        # TODO: Implement texture pooling to reuse textures
        return self.create_texture(
            width=width,
            height=height,
            format=format
        )

    def scale_texture(
        self,
        src_texture: 'wgpu.GPUTexture',
        src_w: int,
        src_h: int,
        dst_w: int,
        dst_h: int
    ) -> 'wgpu.GPUTexture':
        """
        Scale a texture using GPU bilinear interpolation.

        Used by camera capture to scale frames to runtime size.

        Args:
            src_texture: Source wgpu.GPUTexture
            src_w: Source width
            src_h: Source height
            dst_w: Destination width
            dst_h: Destination height

        Returns:
            New wgpu.GPUTexture at dst_w x dst_h

        Example:
            scaled = gpu_ctx.scale_texture(camera_tex, 1280, 720, 1920, 1080)
        """
        device = self.device
        queue = device.queue

        # Create destination texture
        dst = device.create_texture(
            size=(dst_w, dst_h, 1),
            format="bgra8unorm",
            usage=(
                wgpu.TextureUsage.RENDER_ATTACHMENT |
                wgpu.TextureUsage.TEXTURE_BINDING |
                wgpu.TextureUsage.COPY_SRC
            )
        )

        # Create linear sampler (cache on first call)
        if not hasattr(self, '_linear_sampler'):
            self._linear_sampler = device.create_sampler(
                min_filter='linear',
                mag_filter='linear'
            )

        # Create scaling pipeline (cache on first call)
        if not hasattr(self, '_scale_pipeline'):
            # Simple fullscreen blit shader with bilinear sampling
            shader_code = """
            @group(0) @binding(0) var samp: sampler;
            @group(0) @binding(1) var tex: texture_2d<f32>;

            struct VertexOutput {
                @builtin(position) position: vec4<f32>,
                @location(0) uv: vec2<f32>,
            };

            @vertex
            fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
                var out: VertexOutput;
                // Fullscreen triangle
                let x = f32((vertex_index << 1u) & 2u);
                let y = f32(vertex_index & 2u);
                out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
                out.uv = vec2<f32>(x, y);
                return out;
            }

            @fragment
            fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
                return textureSample(tex, samp, in.uv);
            }
            """

            shader_module = device.create_shader_module(code=shader_code)

            bind_group_layout = device.create_bind_group_layout(
                entries=[
                    {
                        "binding": 0,
                        "visibility": wgpu.ShaderStage.FRAGMENT,
                        "sampler": {"type": wgpu.SamplerBindingType.filtering}
                    },
                    {
                        "binding": 1,
                        "visibility": wgpu.ShaderStage.FRAGMENT,
                        "texture": {
                            "sample_type": wgpu.TextureSampleType.float,
                            "view_dimension": wgpu.TextureViewDimension.d2
                        }
                    }
                ]
            )

            pipeline_layout = device.create_pipeline_layout(
                bind_group_layouts=[bind_group_layout]
            )

            self._scale_pipeline = device.create_render_pipeline(
                layout=pipeline_layout,
                vertex={
                    "module": shader_module,
                    "entry_point": "vs_main"
                },
                fragment={
                    "module": shader_module,
                    "entry_point": "fs_main",
                    "targets": [{"format": "bgra8unorm"}]
                },
                primitive={"topology": "triangle-list"}
            )
            self._scale_bind_group_layout = bind_group_layout

        # Create bind group with source texture
        bind_group = device.create_bind_group(
            layout=self._scale_bind_group_layout,
            entries=[
                {"binding": 0, "resource": self._linear_sampler},
                {"binding": 1, "resource": src_texture.create_view()},
            ]
        )

        # Render fullscreen quad with bilinear sampling
        encoder = device.create_command_encoder()
        rp = encoder.begin_render_pass(
            color_attachments=[{
                "view": dst.create_view(),
                "clear_value": (0, 0, 0, 1),
                "load_op": "clear",
                "store_op": "store",
            }]
        )
        rp.set_pipeline(self._scale_pipeline)
        rp.set_bind_group(0, bind_group)
        rp.draw(3, 1, 0, 0)  # 3 vertices for fullscreen triangle
        rp.end()

        queue.submit([encoder.finish()])
        return dst

    def list_cameras(self):
        """
        Enumerate available cameras.

        Returns:
            List of camera info dicts: [{'device_id': '0x...', 'name': 'Camera Name'}, ...]

        Example:
            cameras = gpu_ctx.list_cameras()
            for cam in cameras:
                print(f"{cam['name']}: {cam['device_id']}")
        """
        import sys

        if sys.platform == 'darwin':
            import AVFoundation
            cameras = []
            devices = AVFoundation.AVCaptureDevice.devicesWithMediaType_(
                AVFoundation.AVMediaTypeVideo
            )
            for device in devices:
                cameras.append({
                    'device_id': device.uniqueID(),
                    'name': device.localizedName()
                })
            return cameras

        # TODO: Linux and Windows implementations
        return []

    def create_camera_capture(self, device_id=None):
        """
        Create camera capture that outputs at runtime's frame size.

        Args:
            device_id: Unique camera ID from list_cameras() (None = first available)

        Returns:
            CameraCapture instance

        The capture automatically scales camera frames to runtime.width x runtime.height.
        Call get_texture() to get latest frame (zero-copy on macOS).

        Example:
            # In handler's on_start():
            self.capture = self._runtime.gpu_context.create_camera_capture()

            # In handler's process():
            texture = self.capture.get_texture()  # wgpu.GPUTexture
        """
        from .capture import CameraCapture

        # Get runtime dimensions (stored on GPUContext by StreamRuntime)
        width = getattr(self, '_runtime_width', 1920)
        height = getattr(self, '_runtime_height', 1080)

        return CameraCapture(
            gpu_context=self,
            runtime_width=width,
            runtime_height=height,
            device_id=device_id
        )

    def create_display(
        self,
        width: Optional[int] = None,
        height: Optional[int] = None,
        title: str = "streamlib Display",
        show_fps: bool = False
    ):
        """
        Create display window for rendering GPU textures.

        Args:
            width: Window width (None = use runtime width)
            height: Window height (None = use runtime height)
            title: Window title
            show_fps: If True, display FPS counter in window title

        Returns:
            DisplayWindow instance

        The display window provides a WebGPU swapchain for zero-copy rendering.
        Call render(texture) to display a texture, or use get_current_texture()
        for manual rendering control.

        Example:
            # In handler's on_start():
            self.display = self._runtime.gpu_context.create_display(
                title="My Stream",
                show_fps=True  # Show FPS in window title
            )

            # In handler's process():
            frame = self.inputs['video'].read_latest()
            if frame:
                self.display.render(frame.data)  # Zero-copy to swapchain
        """
        from .display import DisplayWindow

        # Get runtime dimensions if not specified
        if width is None:
            width = getattr(self, '_runtime_width', 1920)
        if height is None:
            height = getattr(self, '_runtime_height', 1080)

        return DisplayWindow(
            gpu_context=self,
            width=width,
            height=height,
            title=title,
            show_fps=show_fps
        )

    def __repr__(self) -> str:
        return (
            f"GPUContext(backend={self.backend_name}, "
            f"device={self.device_name})"
        )
