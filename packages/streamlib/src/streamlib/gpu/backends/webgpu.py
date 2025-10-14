"""
WebGPU backend implementation.

Provides unified GPU access using WebGPU, which automatically selects
the best native backend per platform:
- macOS: Metal
- Windows: Direct3D 12
- Linux: Vulkan
"""

from typing import Optional, Dict, Any
import sys

try:
    import wgpu
    HAS_WGPU = True
except ImportError:
    HAS_WGPU = False
    wgpu = None


class WebGPUBackend:
    """
    WebGPU backend for unified GPU access.

    This backend automatically selects the best GPU API per platform:
    - macOS: Metal (same as before, but unified API)
    - Windows: Direct3D 12
    - Linux: Vulkan

    Example:
        backend = await WebGPUBackend.create()
        print(f"Using {backend.backend_name} on {backend.adapter_info['name']}")
    """

    def __init__(
        self,
        adapter: 'wgpu.GPUAdapter',
        device: 'wgpu.GPUDevice',
        queue: 'wgpu.GPUQueue'
    ):
        """
        Initialize WebGPU backend (use create() instead).

        Args:
            adapter: WebGPU adapter
            device: WebGPU device
            queue: WebGPU command queue
        """
        self.adapter = adapter
        self.device = device
        self.queue = queue

        # Cache adapter info
        self._adapter_info: Optional[Dict[str, Any]] = None

    @classmethod
    async def create(
        cls,
        power_preference: str = 'high-performance',
        canvas: Optional[Any] = None
    ) -> 'WebGPUBackend':
        """
        Create WebGPU backend (async).

        Args:
            power_preference: 'high-performance' or 'low-power'
            canvas: Optional canvas for rendering (None for compute-only)

        Returns:
            WebGPUBackend instance

        Raises:
            RuntimeError: If WebGPU not available or initialization fails

        Example:
            backend = await WebGPUBackend.create()
        """
        if not HAS_WGPU:
            raise RuntimeError(
                "WebGPU not available. Install with: pip install wgpu"
            )

        try:
            # Request adapter (auto-selects best backend)
            adapter = await wgpu.gpu.request_adapter_async(
                power_preference=power_preference,
                canvas=canvas
            )

            if adapter is None:
                raise RuntimeError("Failed to request WebGPU adapter")

            # Request device
            device = await adapter.request_device_async()

            if device is None:
                raise RuntimeError("Failed to request WebGPU device")

            # Get command queue
            queue = device.queue

            return cls(adapter=adapter, device=device, queue=queue)

        except Exception as e:
            raise RuntimeError(f"Failed to initialize WebGPU: {e}") from e

    @property
    def adapter_info(self) -> Dict[str, Any]:
        """
        Get adapter information.

        Returns:
            Dictionary with adapter details:
            - name: GPU name (e.g., "Apple M1 Pro")
            - backend: Backend type (e.g., "Metal", "D3D12", "Vulkan")
            - device_type: "DiscreteGPU", "IntegratedGPU", etc.
        """
        if self._adapter_info is None:
            # Try to get adapter info (API may vary by wgpu version)
            try:
                # Try direct property access first
                description = str(getattr(self.adapter, 'info', 'Unknown GPU'))
            except:
                description = 'Unknown GPU'

            self._adapter_info = {
                'description': description,
                'backend_type': self.backend_name,
            }

        return self._adapter_info

    @property
    def backend_name(self) -> str:
        """
        Get backend name.

        Returns:
            'Metal' (macOS), 'D3D12' (Windows), or 'Vulkan' (Linux)
        """
        # Infer from platform
        if sys.platform == 'darwin':
            return 'Metal'
        elif sys.platform == 'win32':
            return 'D3D12'
        else:
            return 'Vulkan'

    @property
    def limits(self) -> Dict[str, int]:
        """
        Get device limits.

        Returns:
            Dictionary with device limits:
            - max_texture_dimension_2d: Maximum 2D texture size
            - max_buffer_size: Maximum buffer size
            - max_compute_workgroup_size_x/y/z: Max workgroup dimensions
        """
        limits = self.device.limits
        return {
            'max_texture_dimension_2d': limits.get('max_texture_dimension_2d', 8192),
            'max_buffer_size': limits.get('max_buffer_size', 256 * 1024 * 1024),  # 256MB
            'max_compute_workgroup_size_x': limits.get('max_compute_workgroup_size_x', 256),
            'max_compute_workgroup_size_y': limits.get('max_compute_workgroup_size_y', 256),
            'max_compute_workgroup_size_z': limits.get('max_compute_workgroup_size_z', 64),
            'max_compute_invocations_per_workgroup': limits.get('max_compute_invocations_per_workgroup', 256),
        }

    def create_buffer(
        self,
        size: int,
        usage: 'wgpu.BufferUsage',
        label: Optional[str] = None
    ) -> 'wgpu.GPUBuffer':
        """
        Create GPU buffer.

        Args:
            size: Buffer size in bytes
            usage: Buffer usage flags (e.g., wgpu.BufferUsage.STORAGE)
            label: Optional debug label

        Returns:
            WebGPU buffer

        Example:
            buffer = backend.create_buffer(
                size=1920*1080*4,
                usage=wgpu.BufferUsage.STORAGE | wgpu.BufferUsage.COPY_DST
            )
        """
        return self.device.create_buffer(
            size=size,
            usage=usage,
            label=label
        )

    def create_texture(
        self,
        width: int,
        height: int,
        format: str = 'rgba8unorm',
        usage: Optional['wgpu.TextureUsage'] = None,
        label: Optional[str] = None
    ) -> 'wgpu.GPUTexture':
        """
        Create GPU texture.

        Args:
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (default: 'rgba8unorm')
            usage: Texture usage flags (default: TEXTURE_BINDING | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            WebGPU texture

        Example:
            texture = backend.create_texture(
                width=1920,
                height=1080,
                format='rgba8unorm'
            )
        """
        if usage is None:
            usage = (
                wgpu.TextureUsage.TEXTURE_BINDING |
                wgpu.TextureUsage.COPY_DST |
                wgpu.TextureUsage.COPY_SRC
            )

        return self.device.create_texture(
            size=(width, height, 1),
            format=format,
            usage=usage,
            dimension='2d',
            label=label
        )

    def __repr__(self) -> str:
        info = self.adapter_info
        return (
            f"WebGPUBackend(backend={self.backend_name}, "
            f"device={info.get('description', 'Unknown')})"
        )
