"""Capture and save camera frames as PNG images."""
import asyncio
import numpy as np
from PIL import Image
from streamlib.gpu import GPUContext

async def main():
    print("Creating GPU context...")
    ctx = await GPUContext.create()

    print("Creating camera capture...")
    camera = ctx.create_camera_capture()

    print("Waiting for camera to warm up...")
    await asyncio.sleep(2)

    print("Capturing frames...")
    for i in range(5):
        # Get texture
        texture = camera.get_texture()
        print(f"Frame {i+1}: Got texture {texture.size}")

        # Read texture data to CPU
        device = ctx.device

        # Create a buffer to read texture data
        width, height = texture.size[0], texture.size[1]
        bytes_per_pixel = 4  # BGRA
        bytes_per_row = width * bytes_per_pixel
        buffer_size = bytes_per_row * height

        # Create staging buffer
        buffer = device.create_buffer(
            size=buffer_size,
            usage=wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.MAP_READ
        )

        # Copy texture to buffer
        encoder = device.create_command_encoder()
        encoder.copy_texture_to_buffer(
            {
                "texture": texture,
                "mip_level": 0,
                "origin": (0, 0, 0),
            },
            {
                "buffer": buffer,
                "offset": 0,
                "bytes_per_row": bytes_per_row,
                "rows_per_image": height,
            },
            (width, height, 1),
        )
        device.queue.submit([encoder.finish()])

        # Read buffer data
        await buffer.map_async(wgpu.MapMode.READ)
        data = buffer.read_mapped()
        buffer.unmap()

        # Convert BGRA to RGB
        img_array = np.frombuffer(data, dtype=np.uint8).reshape((height, width, 4))
        # Swap B and R channels (BGRA -> RGBA)
        img_array = img_array[:, :, [2, 1, 0, 3]]  # BGRA -> RGBA
        # Drop alpha channel
        img_array = img_array[:, :, :3]  # RGB

        # Save as PNG
        img = Image.fromarray(img_array, mode='RGB')
        filename = f"camera_frame_{i+1}.png"
        img.save(filename)
        print(f"✅ Saved {filename} ({width}x{height})")

        # Wait a bit between captures
        await asyncio.sleep(0.5)

    camera.stop()
    print("\n✅ All frames saved!")

if __name__ == "__main__":
    import wgpu
    asyncio.run(main())
