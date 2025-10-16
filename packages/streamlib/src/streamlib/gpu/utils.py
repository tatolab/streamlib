"""
GPU utility functions for streamlib.

Provides functions for creating test patterns, solid colors, and gradients.
All operations use raw bytes and WebGPU - no numpy dependency.

Example:
    gpu_ctx = await GPUContext.create()
    texture = gpu_ctx.utils.create_test_pattern(640, 480, pattern='smpte_bars')
"""

from typing import Tuple, Literal, Optional, TYPE_CHECKING
import wgpu

if TYPE_CHECKING:
    from .context import GPUContext

TestPatternType = Literal[
    'smpte_bars',
    'checkerboard',
    'gradient',
    'grid',
    'circle',
    'stripes',
]


class GPUUtils:
    """GPU utility functions for texture generation."""

    def __init__(self, context: 'GPUContext'):
        self.context = context

    def create_test_pattern(
        self,
        width: int,
        height: int,
        pattern: TestPatternType = 'smpte_bars'
    ) -> 'wgpu.GPUTexture':
        """Create a test pattern texture."""
        if pattern == 'smpte_bars':
            data = self._generate_smpte_bars(width, height)
        elif pattern == 'checkerboard':
            data = self._generate_checkerboard(width, height)
        elif pattern == 'gradient':
            data = self._generate_gradient(width, height)
        elif pattern == 'grid':
            data = self._generate_grid(width, height)
        elif pattern == 'circle':
            data = self._generate_circle(width, height)
        elif pattern == 'stripes':
            data = self._generate_stripes(width, height)
        else:
            raise ValueError(f"Unknown test pattern: {pattern}")

        texture = self.context.create_texture(width, height)
        self.upload_to_texture(texture, data)
        return texture

    def create_solid_color(
        self,
        width: int,
        height: int,
        color: Tuple[int, int, int, int] = (255, 255, 255, 255)
    ) -> 'wgpu.GPUTexture':
        """Create a solid color texture."""
        size = width * height * 4
        data = bytearray(size)

        for i in range(0, size, 4):
            data[i:i+4] = color

        texture = self.context.create_texture(width, height)
        self.upload_to_texture(texture, data)
        return texture

    def create_gradient(
        self,
        width: int,
        height: int,
        start_color: Tuple[int, int, int, int] = (0, 0, 0, 255),
        end_color: Tuple[int, int, int, int] = (255, 255, 255, 255),
        direction: Literal['horizontal', 'vertical', 'diagonal'] = 'horizontal'
    ) -> 'wgpu.GPUTexture':
        """Create a gradient texture."""
        data = bytearray(width * height * 4)

        if direction == 'horizontal':
            for y in range(height):
                for x in range(width):
                    t = x / (width - 1) if width > 1 else 0
                    idx = (y * width + x) * 4
                    for i in range(4):
                        data[idx + i] = int(start_color[i] * (1 - t) + end_color[i] * t)

        elif direction == 'vertical':
            for y in range(height):
                t = y / (height - 1) if height > 1 else 0
                color = tuple(int(start_color[i] * (1 - t) + end_color[i] * t) for i in range(4))
                for x in range(width):
                    idx = (y * width + x) * 4
                    data[idx:idx+4] = color

        elif direction == 'diagonal':
            for y in range(height):
                for x in range(width):
                    t = (x + y) / (width + height - 2) if (width + height) > 2 else 0
                    idx = (y * width + x) * 4
                    for i in range(4):
                        data[idx + i] = int(start_color[i] * (1 - t) + end_color[i] * t)

        texture = self.context.create_texture(width, height)
        self.upload_to_texture(texture, data)
        return texture

    def upload_to_texture(
        self,
        texture: 'wgpu.GPUTexture',
        data: bytes
    ) -> None:
        """Upload raw bytes to GPU texture."""
        width, height = texture.size[:2]
        expected_size = width * height * 4

        if len(data) != expected_size:
            raise ValueError(f"Data size mismatch: expected {expected_size}, got {len(data)}")

        self.context.queue.write_texture(
            {
                "texture": texture,
                "mip_level": 0,
                "origin": (0, 0, 0)
            },
            data,
            {
                "bytes_per_row": width * 4,
                "rows_per_image": height
            },
            (width, height, 1)
        )

    def download_from_texture(
        self,
        texture: 'wgpu.GPUTexture'
    ) -> bytes:
        """Download GPU texture to bytes."""
        width, height = texture.size[:2]
        buffer_size = width * height * 4

        staging_buffer = self.context.create_buffer(
            size=buffer_size,
            usage=wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.MAP_READ
        )

        encoder = self.context.device.create_command_encoder()
        encoder.copy_texture_to_buffer(
            {
                "texture": texture,
                "mip_level": 0,
                "origin": (0, 0, 0)
            },
            {
                "buffer": staging_buffer,
                "offset": 0,
                "bytes_per_row": width * 4,
                "rows_per_image": height
            },
            (width, height, 1)
        )
        self.context.queue.submit([encoder.finish()])

        staging_buffer.map_sync(mode=wgpu.MapMode.READ)
        data = bytes(staging_buffer.read_mapped())
        staging_buffer.unmap()

        return data

    def _generate_smpte_bars(self, width: int, height: int) -> bytearray:
        """Generate SMPTE color bars pattern."""
        data = bytearray(width * height * 4)

        colors = [
            (192, 192, 192, 255),
            (192, 192, 0, 255),
            (0, 192, 192, 255),
            (0, 192, 0, 255),
            (192, 0, 192, 255),
            (192, 0, 0, 255),
            (0, 0, 192, 255),
            (192, 192, 192, 255),
        ]

        bar_width = width // len(colors)
        for i, color in enumerate(colors):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < len(colors) - 1 else width
            for y in range(height):
                for x in range(x_start, x_end):
                    idx = (y * width + x) * 4
                    data[idx:idx+4] = color

        return data

    def _generate_checkerboard(self, width: int, height: int, square_size: int = 32) -> bytearray:
        """Generate checkerboard pattern."""
        data = bytearray(width * height * 4)

        for y in range(height):
            for x in range(width):
                idx = (y * width + x) * 4
                if ((x // square_size) + (y // square_size)) % 2 == 0:
                    data[idx:idx+4] = (255, 255, 255, 255)
                else:
                    data[idx:idx+4] = (0, 0, 0, 255)

        return data

    def _generate_gradient(self, width: int, height: int) -> bytearray:
        """Generate horizontal gradient pattern."""
        data = bytearray(width * height * 4)

        for y in range(height):
            for x in range(width):
                t = x / (width - 1) if width > 1 else 0
                idx = (y * width + x) * 4
                val = int(255 * t)
                data[idx:idx+4] = (val, val, val, 255)

        return data

    def _generate_grid(self, width: int, height: int, grid_size: int = 32) -> bytearray:
        """Generate grid lines pattern."""
        data = bytearray(width * height * 4)

        for y in range(height):
            for x in range(width):
                idx = (y * width + x) * 4
                if x % grid_size == 0 or y % grid_size == 0:
                    data[idx:idx+4] = (128, 128, 128, 255)
                else:
                    data[idx:idx+4] = (32, 32, 32, 255)

        return data

    def _generate_circle(self, width: int, height: int) -> bytearray:
        """Generate centered circle pattern."""
        data = bytearray(width * height * 4)
        center_x, center_y = width // 2, height // 2
        radius = min(width, height) // 3

        for y in range(height):
            for x in range(width):
                dist = ((x - center_x) ** 2 + (y - center_y) ** 2) ** 0.5
                idx = (y * width + x) * 4
                if dist <= radius:
                    data[idx:idx+4] = (255, 255, 255, 255)
                else:
                    data[idx:idx+4] = (0, 0, 0, 255)

        return data

    def _generate_stripes(self, width: int, height: int, stripe_width: int = 16) -> bytearray:
        """Generate vertical stripes pattern."""
        data = bytearray(width * height * 4)

        for y in range(height):
            for x in range(width):
                idx = (y * width + x) * 4
                if (x // stripe_width) % 2 == 0:
                    data[idx:idx+4] = (255, 255, 255, 255)
                else:
                    data[idx:idx+4] = (0, 0, 0, 255)

        return data
