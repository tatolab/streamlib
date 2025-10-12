#!/usr/bin/env python3
"""
Minimal test to isolate camera capture and CVPixelBuffer reading.
"""

import time
import numpy as np
import cv2

try:
    import objc
    from Foundation import NSObject, NSRunLoop, NSDefaultRunLoopMode, NSDate
    from AVFoundation import (
        AVCaptureSession,
        AVCaptureDevice,
        AVCaptureDeviceInput,
        AVCaptureVideoDataOutput,
        AVMediaTypeVideo,
        AVCaptureSessionPreset1920x1080,
    )
    from Quartz.CoreVideo import (
        kCVPixelFormatType_32BGRA,
        kCVPixelBufferPixelFormatTypeKey,
        CVPixelBufferLockBaseAddress,
        CVPixelBufferUnlockBaseAddress,
        CVPixelBufferGetBaseAddress,
        CVPixelBufferGetBytesPerRow,
        CVPixelBufferGetWidth,
        CVPixelBufferGetHeight,
    )
    from CoreMedia import CMSampleBufferGetImageBuffer
    import ctypes
except ImportError as e:
    print(f"Import error: {e}")
    exit(1)


class SimpleCameraDelegate(NSObject):
    """Minimal delegate to capture frames."""

    def init(self):
        self = objc.super(SimpleCameraDelegate, self).init()
        if self is None:
            return None
        self.frame_buffer = None
        self.frame_count = 0
        return self

    def captureOutput_didOutputSampleBuffer_fromConnection_(
        self, output, sample_buffer, connection
    ):
        """Called when a new frame is available."""
        pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer)
        if pixel_buffer:
            self.frame_buffer = pixel_buffer
            self.frame_count += 1
            if self.frame_count <= 3:
                print(f"[Delegate] Frame {self.frame_count} received")


def read_pixel_buffer(pixel_buffer):
    """Attempt to read CVPixelBuffer to numpy array."""

    # Get dimensions
    width = CVPixelBufferGetWidth(pixel_buffer)
    height = CVPixelBufferGetHeight(pixel_buffer)

    print(f"\n=== Buffer Info ===")
    print(f"Width: {width}, Height: {height}")

    # Lock buffer
    CVPixelBufferLockBaseAddress(pixel_buffer, 0)

    try:
        # Get buffer properties
        base_address = CVPixelBufferGetBaseAddress(pixel_buffer)
        bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer)

        print(f"Bytes per row: {bytes_per_row}")
        print(f"Expected (width*4): {width * 4}")
        print(f"Has padding: {bytes_per_row > width * 4}")

        # Try different pointer conversion methods
        print(f"\n=== Pointer Conversion ===")
        print(f"base_address type: {type(base_address)}")
        print(f"base_address: {base_address}")

        # For objc.varlist, we need to create a numpy array DIRECTLY from it
        # PyObjC provides special support for varlist → numpy conversion
        print(f"\n=== Direct numpy conversion from varlist ===")

        # Try using objc's buffer protocol
        total_bytes = height * bytes_per_row
        frame_1d = None

        # Method 1: Try memoryview
        try:
            print(f"Attempting memoryview...")
            mv = memoryview(base_address)
            frame_1d = np.frombuffer(mv, dtype=np.uint8, count=total_bytes)
            print(f"✓ memoryview worked! Shape: {frame_1d.shape}")
            ptr_value = "memoryview"
        except Exception as e:
            print(f"memoryview failed: {e}")

        # Method 2: Try converting varlist to bytes
        if frame_1d is None:
            try:
                print(f"Attempting bytes conversion...")
                # varlist should implement __bytes__ or similar
                buffer_bytes = bytes(base_address)
                frame_1d = np.frombuffer(buffer_bytes, dtype=np.uint8, count=total_bytes)
                print(f"✓ bytes() worked! Shape: {frame_1d.shape}")
                ptr_value = "bytes"
            except Exception as e:
                print(f"bytes conversion failed: {e}")

        # Method 3: Try directly accessing buffer interface
        if frame_1d is None:
            try:
                print(f"Attempting __array_interface__...")
                if hasattr(base_address, '__array_interface__'):
                    print(f"  __array_interface__: {base_address.__array_interface__}")
                    frame_1d = np.array(base_address, copy=False)
                    print(f"✓ __array_interface__ worked! Shape: {frame_1d.shape}")
                    ptr_value = "array_interface"
                else:
                    print(f"  No __array_interface__")
            except Exception as e:
                print(f"__array_interface__ failed: {e}")

        if frame_1d is None:
            ptr_value = None

        if not ptr_value:
            raise RuntimeError("Could not convert pointer")

        print(f"\n=== Using method: {ptr_value} ===")

        # We have the numpy array from one of the methods
        # Read first 64 bytes to inspect
        first_bytes = frame_1d[:64].tolist()

        print(f"\n=== First 64 bytes ===")
        print(" ".join(f"{b:02x}" for b in first_bytes))

        # Check if it looks like valid BGRA data
        unique_values = len(set(first_bytes))
        print(f"\nUnique values in first 64 bytes: {unique_values}")

        if unique_values < 5:
            print("⚠️  WARNING: Very low variation - might be reading wrong memory!")

        # Reshape to 2D
        frame_bytes = frame_1d.reshape(height, bytes_per_row)

        # Reshape accounting for stride
        stride_pixels = bytes_per_row // 4
        frame_bgra = frame_bytes.reshape(height, stride_pixels, 4)[:, :width, :]

        # Convert BGRA → RGB
        frame_rgb = frame_bgra[:, :, [2, 1, 0]].copy()

        print(f"\n=== Frame Stats ===")
        print(f"Shape: {frame_rgb.shape}")
        print(f"Dtype: {frame_rgb.dtype}")
        print(f"Min: {frame_rgb.min()}, Max: {frame_rgb.max()}, Mean: {frame_rgb.mean():.1f}")

        return frame_rgb

    finally:
        CVPixelBufferUnlockBaseAddress(pixel_buffer, 0)


def main():
    print("=== Simple Camera Capture Test ===\n")

    # Find FaceTime HD Camera
    devices = AVCaptureDevice.devicesWithMediaType_(AVMediaTypeVideo)
    camera = None
    for device in devices:
        if "FaceTime HD Camera" in device.localizedName():
            camera = device
            break

    if not camera:
        print("❌ FaceTime HD Camera not found!")
        return

    print(f"✓ Found camera: {camera.localizedName()}")

    # Create capture session
    session = AVCaptureSession.alloc().init()
    session.setSessionPreset_(AVCaptureSessionPreset1920x1080)

    # Add camera input
    device_input, error = AVCaptureDeviceInput.deviceInputWithDevice_error_(camera, None)
    if error:
        print(f"❌ Failed to create input: {error}")
        return

    session.addInput_(device_input)

    # Create video output with BGRA format
    output = AVCaptureVideoDataOutput.alloc().init()
    settings = {
        kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA
    }
    output.setVideoSettings_(settings)
    output.setAlwaysDiscardsLateVideoFrames_(True)

    # Create delegate
    delegate = SimpleCameraDelegate.alloc().init()

    # Set up dispatch queue
    libdispatch = ctypes.CDLL('/usr/lib/system/libdispatch.dylib')
    dispatch_get_global_queue = libdispatch.dispatch_get_global_queue
    dispatch_get_global_queue.restype = ctypes.c_void_p
    dispatch_get_global_queue.argtypes = [ctypes.c_long, ctypes.c_ulong]

    queue_ptr = dispatch_get_global_queue(0, 0)
    queue = objc.objc_object(c_void_p=queue_ptr)

    output.setSampleBufferDelegate_queue_(delegate, queue)
    session.addOutput_(output)

    # Start capture in background thread with run loop
    import threading

    def capture_thread():
        session.startRunning()
        print("✓ Camera started\n")
        run_loop = NSRunLoop.currentRunLoop()
        while session.isRunning():
            run_loop.runMode_beforeDate_(NSDefaultRunLoopMode, NSDate.dateWithTimeIntervalSinceNow_(0.1))

    thread = threading.Thread(target=capture_thread, daemon=True)
    thread.start()

    # Wait for frames to arrive
    print("Waiting for frames...")
    for i in range(50):  # Wait up to 5 seconds
        if delegate.frame_count >= 3:
            break
        time.sleep(0.1)

    if delegate.frame_count == 0:
        print("❌ No frames captured!")
        session.stopRunning()
        return

    print(f"\n✓ Captured {delegate.frame_count} frames")

    # Read and save the latest frame
    if delegate.frame_buffer:
        try:
            frame_rgb = read_pixel_buffer(delegate.frame_buffer)

            # Save frame
            output_path = "/tmp/test_camera_simple.png"
            cv2.imwrite(output_path, cv2.cvtColor(frame_rgb, cv2.COLOR_RGB2BGR))
            print(f"\n✓ Saved frame to {output_path}")

        except Exception as e:
            print(f"\n❌ Error reading buffer: {e}")
            import traceback
            traceback.print_exc()

    # Stop
    session.stopRunning()
    print("\n✓ Test complete!")


if __name__ == '__main__':
    main()
