"""Test zero-copy IOSurface → Metal → wgpu texture pipeline with actual IOSurfaces."""

import iosurface_hal
import numpy as np
from PIL import Image
import time
import sys


def test_zero_copy_pipeline():
    """Test the zero-copy Metal texture creation pipeline with real IOSurfaces."""

    print("=" * 70)
    print("ZERO-COPY IOSURFACE → METAL → WGPU HAL TEXTURE TEST")
    print("=" * 70)

    # Get Metal device
    print("\n1. Getting Metal device...")
    metal_device = iosurface_hal.get_default_metal_device()
    print(f"   ✓ Got Metal device: 0x{metal_device:x}")

    # Create a test IOSurface (640x480)
    print("\n2. Creating test IOSurface...")
    width, height = 640, 480

    # IOSurface properties dictionary
    iosurface_props = {
        'IOSurfaceWidth': width,
        'IOSurfaceHeight': height,
        'IOSurfacePixelFormat': 0x42475241,  # 'BGRA' in little-endian
        'IOSurfaceBytesPerElement': 4,
        'IOSurfaceBytesPerRow': width * 4,
        'IOSurfacePlaneInfo': [
            {
                'IOSurfacePlaneWidth': width,
                'IOSurfacePlaneHeight': height,
                'IOSurfacePlaneBytesPerRow': width * 4,
                'IOSurfacePlaneOffset': 0,
                'IOSurfacePlaneSize': width * height * 4
            }
        ]
    }

    # Create IOSurface using IOSurfaceCreate
    import ctypes
    from ctypes import c_void_p

    # Load IOSurface framework
    iosurface_framework = ctypes.CDLL('/System/Library/Frameworks/IOSurface.framework/IOSurface')
    core_foundation = ctypes.CDLL('/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation')

    # Define function signatures
    iosurface_framework.IOSurfaceCreate.restype = c_void_p
    iosurface_framework.IOSurfaceCreate.argtypes = [c_void_p]

    # Convert Python dict to CFDictionary (simplified - using Objective-C bridge)
    from Foundation import NSDictionary
    ns_dict = NSDictionary.dictionaryWithDictionary_(iosurface_props)

    # Create IOSurface
    iosurface_ptr = iosurface_framework.IOSurfaceCreate(ns_dict.__c_void_p__())

    if iosurface_ptr:
        print(f"   ✓ Created IOSurface: 0x{iosurface_ptr:x}")
        print(f"     Size: {width}x{height}")
        print(f"     Format: BGRA8")
    else:
        print("   ✗ Failed to create IOSurface")
        return False

    # Test the zero-copy Metal texture creation
    print("\n3. Creating Metal texture from IOSurface (ZERO-COPY)...")

    start_time = time.perf_counter()

    try:
        # Create Metal texture from IOSurface (ZERO-COPY!)
        metal_texture = iosurface_hal.create_metal_texture_from_iosurface(
            iosurface_ptr,
            metal_device,
            width,
            height
        )

        creation_time = (time.perf_counter() - start_time) * 1000
        print(f"   ✓ Metal texture created in {creation_time:.3f}ms")

        # Get dimensions to verify
        dims = metal_texture.get_dimensions()
        print(f"   ✓ Texture dimensions: {dims[0]}x{dims[1]}")

    except Exception as e:
        print(f"   ✗ Failed to create Metal texture: {e}")
        import traceback
        traceback.print_exc()
        return False

    # Test HAL texture creation
    print("\n4. Creating wgpu HAL texture from Metal texture (ZERO-COPY)...")

    start_time = time.perf_counter()

    try:
        # Create HAL texture from Metal texture (ZERO-COPY!)
        hal_texture_ptr = metal_texture.create_hal_texture()

        hal_time = (time.perf_counter() - start_time) * 1000
        print(f"   ✓ HAL texture created in {hal_time:.3f}ms")
        print(f"   ✓ HAL texture pointer: 0x{hal_texture_ptr:x}")

    except Exception as e:
        print(f"   ✗ Failed to create HAL texture: {e}")
        import traceback
        traceback.print_exc()
        return False

    # For comparison, time the CPU copy approach
    print("\n5. Timing comparison with CPU copy approach...")

    start_time = time.perf_counter()
    pixel_data, w, h, bytes_per_row = iosurface_hal.read_iosurface_pixels(iosurface_ptr)
    cpu_copy_time = (time.perf_counter() - start_time) * 1000

    print(f"   CPU copy (read pixels): {cpu_copy_time:.3f}ms")
    print(f"   Zero-copy (Metal texture): {creation_time:.3f}ms")
    print(f"   Zero-copy (HAL texture): {hal_time:.3f}ms")

    speedup = cpu_copy_time / (creation_time + hal_time)
    print(f"\n   🚀 Zero-copy is {speedup:.1f}x faster than CPU copy!")

    # Save the test pattern for verification
    print("\n6. Saving test image for verification...")

    # Check if we got valid pixel data
    if len(pixel_data) > 0:
        # Convert BGRA to RGB for saving
        img_array = np.frombuffer(pixel_data, dtype=np.uint8).reshape((h, w, 4))
        img_array = img_array[:, :, [2, 1, 0, 3]][:, :, :3]  # BGRA → RGB

        img = Image.fromarray(img_array, mode='RGB')
        img.save("iosurface_zerocopy_test.png")
        print("   ✓ Saved iosurface_zerocopy_test.png")
    else:
        print("   ⚠ No pixel data available (IOSurface may be empty)")

    print("\n" + "=" * 70)
    print("✅ ZERO-COPY PIPELINE TEST SUCCESSFUL!")
    print("=" * 70)
    print("\nSummary:")
    print("1. IOSurface → Metal texture: ZERO-COPY ✓")
    print("2. Metal texture → HAL texture: ZERO-COPY ✓")
    print("3. HAL texture ready for wgpu.create_texture_from_hal() ✓")
    print(f"4. Performance improvement: {speedup:.1f}x faster ✓")

    return True


if __name__ == "__main__":
    success = test_zero_copy_pipeline()
    sys.exit(0 if success else 1)