"""
Compute shader wrapper for WebGPU.

Provides high-level API for loading, compiling, and dispatching compute shaders.
"""

from typing import TYPE_CHECKING, Dict, List, Optional, Tuple, Any
import struct

if TYPE_CHECKING:
    import wgpu
    from .context import GPUContext

try:
    import wgpu
    HAS_WGPU = True
except ImportError:
    HAS_WGPU = False


class ComputeShader:
    """
    WebGPU compute shader wrapper.

    Provides high-level methods for loading WGSL shaders and dispatching compute operations.

    Example:
        # Create shader from WGSL code
        shader = ComputeShader.from_wgsl(
            gpu_ctx,
            shader_code=BLUR_SHADER_WGSL,
            entry_point='blur_main',
            bindings=[
                {'binding': 0, 'type': 'storage', 'access': 'read'},       # input buffer
                {'binding': 1, 'type': 'storage', 'access': 'read_write'}, # output buffer
                {'binding': 2, 'type': 'uniform'},                         # params
            ]
        )

        # Dispatch compute work
        shader.dispatch(
            workgroup_count=(120, 68, 1),  # For 1920x1080 with workgroup_size=(16, 16)
            bindings={
                0: input_buffer,
                1: output_buffer,
                2: params_buffer,
            }
        )
    """

    def __init__(
        self,
        context: 'GPUContext',
        pipeline: 'wgpu.GPUComputePipeline',
        bind_group_layout: 'wgpu.GPUBindGroupLayout'
    ):
        """
        Initialize compute shader (use from_wgsl() instead).

        Args:
            context: GPU context
            pipeline: WebGPU compute pipeline
            bind_group_layout: Bind group layout for shader bindings
        """
        self.context = context
        self.pipeline = pipeline
        self.bind_group_layout = bind_group_layout

    @classmethod
    def from_wgsl(
        cls,
        context: 'GPUContext',
        shader_code: str,
        entry_point: str = 'main',
        bindings: Optional[List[Dict[str, Any]]] = None
    ) -> 'ComputeShader':
        """
        Create compute shader from WGSL code.

        Args:
            context: GPU context
            shader_code: WGSL shader source code
            entry_point: Shader entry point function name (default: 'main')
            bindings: List of binding descriptors:
                [
                    {'binding': 0, 'type': 'storage', 'access': 'read'},
                    {'binding': 1, 'type': 'storage', 'access': 'read_write'},
                    {'binding': 2, 'type': 'uniform'},
                ]

        Returns:
            ComputeShader instance

        Example:
            shader = ComputeShader.from_wgsl(
                gpu_ctx,
                shader_code=BLUR_SHADER_WGSL,
                entry_point='blur_main'
            )
        """
        if not HAS_WGPU:
            raise RuntimeError("WebGPU not available")

        # Create shader module
        shader_module = context.device.create_shader_module(code=shader_code)

        # Create bind group layout from bindings descriptor
        if bindings is None:
            bindings = []

        bind_group_layout_entries = []
        for binding_desc in bindings:
            binding_num = binding_desc['binding']
            binding_type = binding_desc.get('type', 'storage')
            access = binding_desc.get('access', 'read_write')

            if binding_type == 'storage':
                # Storage buffer
                if access == 'read':
                    buffer_type = wgpu.BufferBindingType.read_only_storage
                elif access == 'read_write':
                    buffer_type = wgpu.BufferBindingType.storage
                else:
                    raise ValueError(f"Invalid storage access: {access}")

                bind_group_layout_entries.append({
                    "binding": binding_num,
                    "visibility": wgpu.ShaderStage.COMPUTE,
                    "buffer": {
                        "type": buffer_type,
                    }
                })
            elif binding_type == 'uniform':
                # Uniform buffer
                bind_group_layout_entries.append({
                    "binding": binding_num,
                    "visibility": wgpu.ShaderStage.COMPUTE,
                    "buffer": {
                        "type": wgpu.BufferBindingType.uniform,
                    }
                })
            else:
                raise ValueError(f"Unsupported binding type: {binding_type}")

        bind_group_layout = context.device.create_bind_group_layout(
            entries=bind_group_layout_entries
        )

        # Create pipeline layout
        pipeline_layout = context.device.create_pipeline_layout(
            bind_group_layouts=[bind_group_layout]
        )

        # Create compute pipeline
        pipeline = context.device.create_compute_pipeline(
            layout=pipeline_layout,
            compute={
                "module": shader_module,
                "entry_point": entry_point,
            }
        )

        return cls(
            context=context,
            pipeline=pipeline,
            bind_group_layout=bind_group_layout
        )

    def dispatch(
        self,
        workgroup_count: Tuple[int, int, int],
        bindings: Dict[int, 'wgpu.GPUBuffer']
    ) -> None:
        """
        Dispatch compute shader.

        Args:
            workgroup_count: Number of workgroups to dispatch (x, y, z)
            bindings: Dictionary mapping binding number to GPU buffer
                {
                    0: input_buffer,
                    1: output_buffer,
                    2: params_buffer,
                }

        Example:
            shader.dispatch(
                workgroup_count=(120, 68, 1),  # For 1920x1080 with workgroup_size=(16, 16)
                bindings={
                    0: input_buffer,
                    1: output_buffer,
                    2: params_buffer,
                }
            )
        """
        # Create bind group
        bind_group_entries = []
        for binding_num, buffer in bindings.items():
            bind_group_entries.append({
                "binding": binding_num,
                "resource": {"buffer": buffer, "offset": 0, "size": buffer.size}
            })

        bind_group = self.context.device.create_bind_group(
            layout=self.bind_group_layout,
            entries=bind_group_entries
        )

        # Create command encoder
        encoder = self.context.device.create_command_encoder()

        # Begin compute pass
        compute_pass = encoder.begin_compute_pass()
        compute_pass.set_pipeline(self.pipeline)
        compute_pass.set_bind_group(0, bind_group)
        compute_pass.dispatch_workgroups(*workgroup_count)
        compute_pass.end()

        # Submit command buffer
        self.context.queue.submit([encoder.finish()])

    def __repr__(self) -> str:
        return f"ComputeShader(pipeline={self.pipeline})"


def create_uniform_buffer(
    context: 'GPUContext',
    data: Dict[str, Any],
    layout: List[Tuple[str, str]]
) -> 'wgpu.GPUBuffer':
    """
    Create uniform buffer from Python data.

    Args:
        context: GPU context
        data: Dictionary of uniform values
        layout: List of (name, type) tuples defining struct layout:
            [
                ('width', 'u32'),
                ('height', 'u32'),
                ('kernel_size', 'u32'),
                ('sigma', 'f32'),
            ]

    Returns:
        WebGPU buffer with packed uniform data

    Example:
        params_buffer = create_uniform_buffer(
            gpu_ctx,
            data={'width': 1920, 'height': 1080, 'kernel_size': 15, 'sigma': 3.0},
            layout=[
                ('width', 'u32'),
                ('height', 'u32'),
                ('kernel_size', 'u32'),
                ('sigma', 'f32'),
            ]
        )
    """
    # Pack data according to WGSL alignment rules
    # https://www.w3.org/TR/WGSL/#alignment-and-size
    struct_format = '<'  # Little-endian
    values = []

    for name, dtype in layout:
        value = data[name]

        if dtype == 'u32':
            struct_format += 'I'  # Unsigned int (4 bytes)
            values.append(int(value))
        elif dtype == 'i32':
            struct_format += 'i'  # Signed int (4 bytes)
            values.append(int(value))
        elif dtype == 'f32':
            struct_format += 'f'  # Float (4 bytes)
            values.append(float(value))
        else:
            raise ValueError(f"Unsupported uniform type: {dtype}")

    # Pack into bytes
    packed_data = struct.pack(struct_format, *values)

    # Create buffer
    buffer = context.create_buffer(
        size=len(packed_data),
        usage=wgpu.BufferUsage.UNIFORM | wgpu.BufferUsage.COPY_DST
    )

    # Upload data
    context.queue.write_buffer(buffer, 0, packed_data)

    return buffer
