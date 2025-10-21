"""macOS camera capture using AVFoundation with single-copy pipeline.

This module provides AVFoundationCapture which:
- Captures camera frames using AVFoundation on background thread
- Extracts IOSurface from CVPixelBuffer (GPU memory)
- Writes DIRECTLY from IOSurface to wgpu.GPUTexture (single copy!)
- Outputs frames at runtime.width x runtime.height (scales if needed)
- Returns black frames on disconnection (doesn't crash)

Single-copy pipeline:
1. Lock IOSurface for CPU read access
2. Get base address pointer (points to GPU VRAM)
3. device.queue.write_texture() directly from IOSurface memory
4. Unlock IOSurface

This eliminates the intermediate CPU buffer, reducing from 2 copies to 1 copy.
"""

import threading
import AVFoundation
from Foundation import NSObject, NSNotificationCenter
from Quartz import CVPixelBufferGetIOSurface, kCVPixelFormatType_32BGRA
from CoreMedia import CMSampleBufferGetImageBuffer
import wgpu
import ctypes
import objc

# Import IOSurface functions via ctypes (IOSurface framework)
# These are not in Quartz, need direct framework access
IOSurface = ctypes.CDLL('/System/Library/Frameworks/IOSurface.framework/IOSurface')

# Define IOSurface function signatures
IOSurfaceGetWidth = IOSurface.IOSurfaceGetWidth
IOSurfaceGetWidth.restype = ctypes.c_size_t
IOSurfaceGetWidth.argtypes = [ctypes.c_void_p]

IOSurfaceGetHeight = IOSurface.IOSurfaceGetHeight
IOSurfaceGetHeight.restype = ctypes.c_size_t
IOSurfaceGetHeight.argtypes = [ctypes.c_void_p]

IOSurfaceGetBytesPerRow = IOSurface.IOSurfaceGetBytesPerRow
IOSurfaceGetBytesPerRow.restype = ctypes.c_size_t
IOSurfaceGetBytesPerRow.argtypes = [ctypes.c_void_p]

IOSurfaceGetBaseAddress = IOSurface.IOSurfaceGetBaseAddress
IOSurfaceGetBaseAddress.restype = ctypes.c_void_p
IOSurfaceGetBaseAddress.argtypes = [ctypes.c_void_p]

IOSurfaceLock = IOSurface.IOSurfaceLock
IOSurfaceLock.restype = ctypes.c_int32
IOSurfaceLock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]

IOSurfaceUnlock = IOSurface.IOSurfaceUnlock
IOSurfaceUnlock.restype = ctypes.c_int32
IOSurfaceUnlock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]

# IOSurface lock flags
kIOSurfaceLockReadOnly = 0x00000001


# Get global dispatch queue using ctypes (proven working approach from camera_gpu.py)
def _get_global_dispatch_queue():
    """Get global dispatch queue for camera callbacks."""
    libdispatch = ctypes.CDLL('/usr/lib/system/libdispatch.dylib')
    dispatch_get_global_queue = libdispatch.dispatch_get_global_queue
    dispatch_get_global_queue.restype = ctypes.c_void_p
    dispatch_get_global_queue.argtypes = [ctypes.c_long, ctypes.c_ulong]

    # DISPATCH_QUEUE_PRIORITY_DEFAULT = 0
    queue_ptr = dispatch_get_global_queue(0, 0)

    # CRITICAL: Wrap in objc.objc_object for PyObjC interop!
    queue = objc.objc_object(c_void_p=queue_ptr)
    return queue


class CameraDelegate(NSObject):
    """AVFoundation delegate that receives frames on background thread."""

    def init(self):
        """Initialize delegate."""
        self = objc.super(CameraDelegate, self).init()
        if self is None:
            return None
        self.on_frame = None
        return self

    def captureOutput_didOutputSampleBuffer_fromConnection_(
        self, output, sampleBuffer, connection
    ):
        """AVCaptureVideoDataOutputSampleBufferDelegate callback."""
        try:
            # Get CVPixelBuffer from sample buffer (use CoreMedia, not AVFoundation!)
            pixel_buffer = CMSampleBufferGetImageBuffer(sampleBuffer)

            # Extract IOSurface (zero-copy reference to GPU memory)
            iosurface = CVPixelBufferGetIOSurface(pixel_buffer)

            if iosurface is None:
                raise RuntimeError("Camera frame not backed by IOSurface")

            # Pass to capture handler
            if self.on_frame:
                self.on_frame(iosurface)
        except Exception as e:
            # Don't crash on frame errors
            print(f"Camera frame error: {e}")
            import traceback
            traceback.print_exc()


class AVFoundationCapture:
    """
    macOS camera capture using AVFoundation with single-copy pipeline.

    Outputs frames at runtime's width/height (auto-scales camera).
    Single-copy pipeline: IOSurface → wgpu.GPUTexture (direct write!)

    Uses IOSurface lock/unlock to write directly from GPU memory to wgpu texture.
    """

    def __init__(self, gpu_context, runtime_width, runtime_height, device_id=None):
        """
        Args:
            gpu_context: GPUContext instance
            runtime_width: Runtime frame width
            runtime_height: Runtime frame height
            device_id: Unique camera ID (None = first available)
        """
        self.gpu_context = gpu_context
        self.width = runtime_width
        self.height = runtime_height
        self._latest_texture = None
        self._texture_lock = threading.Lock()
        self._camera_connected = True
        self._black_texture = None  # For disconnection fallback

        # Create capture session
        self.session = AVFoundation.AVCaptureSession.alloc().init()

        # Get camera device
        if device_id:
            device = AVFoundation.AVCaptureDevice.deviceWithUniqueID_(device_id)
            if device is None:
                raise RuntimeError(f"No camera found with device_id '{device_id}'")
        else:
            device = AVFoundation.AVCaptureDevice.defaultDeviceWithMediaType_(
                AVFoundation.AVMediaTypeVideo
            )
            if device is None:
                raise RuntimeError("No camera found")

        # Create device input
        input_device = AVFoundation.AVCaptureDeviceInput.deviceInputWithDevice_error_(
            device, None
        )[0]
        self.session.addInput_(input_device)

        # Create video output (IOSurface-backed automatically for BGRA pixel format)
        output = AVFoundation.AVCaptureVideoDataOutput.alloc().init()
        output.setVideoSettings_({
            'PixelFormatType': kCVPixelFormatType_32BGRA
        })

        # Set delegate for frame callbacks (runs on background thread)
        self.delegate = CameraDelegate.alloc().init()
        self.delegate.on_frame = self._handle_frame

        # Use global dispatch queue for callbacks (proven working approach)
        self.queue = _get_global_dispatch_queue()  # Retain reference to prevent GC!
        output.setSampleBufferDelegate_queue_(self.delegate, self.queue)

        self.session.addOutput_(output)

        # Register for disconnection notifications
        self._register_disconnection_handler()

        # Start capture
        self.session.startRunning()

        # Give camera a moment to warm up
        import time
        time.sleep(0.5)

    def _register_disconnection_handler(self):
        """Register notification handler for camera disconnection."""
        nc = NSNotificationCenter.defaultCenter()

        # Add observer for disconnection notification
        nc.addObserver_selector_name_object_(
            self.delegate,
            'handleDeviceDisconnected:',
            'AVCaptureDeviceWasDisconnectedNotification',
            None
        )

        # Add disconnection handler to delegate
        def handle_disconnected(notification):
            self._camera_connected = False

        self.delegate.handleDeviceDisconnected_ = handle_disconnected

    def _create_black_texture(self):
        """Create solid black texture for disconnection fallback."""
        if self._black_texture is None:
            device = self.gpu_context.device

            # Create texture
            texture = device.create_texture(
                size=(self.width, self.height, 1),
                format='bgra8unorm',
                usage=wgpu.TextureUsage.COPY_DST | wgpu.TextureUsage.COPY_SRC | wgpu.TextureUsage.TEXTURE_BINDING
            )

            # Create zeroed bytes buffer (BGRA format, all zeros = black)
            bytes_per_pixel = 4
            row_bytes = bytes_per_pixel * self.width
            black_data = bytes([0]) * (row_bytes * self.height)

            # Upload zeros to texture
            device.queue.write_texture(
                {"texture": texture},
                black_data,
                {"bytes_per_row": row_bytes, "rows_per_image": self.height},
                (self.width, self.height, 1)
            )

            self._black_texture = texture

        return self._black_texture

    def _handle_frame(self, iosurface):
        """
        Called on background thread by AVFoundation.

        Single-copy pipeline: IOSurface → wgpu texture (direct write!)
        Lock IOSurface → get base address → write_texture → unlock
        """
        try:
            # Get IOSurface as void pointer for C API
            iosurface_ptr = objc.pyobjc_id(iosurface)

            # Get IOSurface dimensions
            camera_width = IOSurfaceGetWidth(iosurface_ptr)
            camera_height = IOSurfaceGetHeight(iosurface_ptr)
            row_bytes = IOSurfaceGetBytesPerRow(iosurface_ptr)

            # Lock IOSurface for CPU read access
            # This ensures thread-safety but doesn't copy data
            lock_result = IOSurfaceLock(iosurface_ptr, kIOSurfaceLockReadOnly, None)
            if lock_result != 0:
                raise RuntimeError(f"Failed to lock IOSurface: {lock_result}")

            try:
                # Get base address pointer (points directly to GPU VRAM!)
                base_address = IOSurfaceGetBaseAddress(iosurface_ptr)
                if not base_address:
                    raise RuntimeError("IOSurface has no base address")

                # Calculate total buffer size
                buffer_size = row_bytes * camera_height

                # Create ctypes buffer from IOSurface memory (single copy!)
                # This creates a Python buffer wrapping the IOSurface memory
                buffer = (ctypes.c_ubyte * buffer_size).from_address(base_address)

                # Create wgpu texture at camera resolution
                camera_texture = self.gpu_context.device.create_texture(
                    size=(camera_width, camera_height, 1),
                    format='bgra8unorm',
                    usage=wgpu.TextureUsage.COPY_DST | wgpu.TextureUsage.COPY_SRC | wgpu.TextureUsage.TEXTURE_BINDING
                )

                # Write DIRECTLY from IOSurface memory to wgpu texture (single copy!)
                # buffer wraps the IOSurface memory without copying
                self.gpu_context.device.queue.write_texture(
                    {"texture": camera_texture},
                    buffer,  # Direct IOSurface memory access via ctypes buffer!
                    {"bytes_per_row": row_bytes, "rows_per_image": camera_height},
                    (camera_width, camera_height, 1)
                )
            finally:
                # Always unlock IOSurface
                IOSurfaceUnlock(iosurface_ptr, kIOSurfaceLockReadOnly, None)

            # Check if scaling is needed
            if camera_width != self.width or camera_height != self.height:
                # Scale to runtime size using GPU
                scaled_texture = self.gpu_context.scale_texture(
                    camera_texture,
                    camera_width,
                    camera_height,
                    self.width,
                    self.height
                )

                # Clean up intermediate camera texture
                camera_texture.destroy()

                new_texture = scaled_texture
            else:
                # Already correct size, use directly
                new_texture = camera_texture

            # Thread-safe update (don't destroy old textures - they may still be in use)
            # Let WebGPU handle cleanup when textures are no longer referenced
            with self._texture_lock:
                # Keep a reference to old texture temporarily to avoid race condition
                # where texture gets destroyed while being read by user code
                _old_texture = self._latest_texture  # Keep reference to prevent premature cleanup
                self._latest_texture = new_texture
                # TODO: Implement proper texture pooling to avoid memory growth

        except Exception as e:
            # Camera error - log but don't crash
            print(f"Camera frame processing error: {e}")
            import traceback
            traceback.print_exc()
            self._camera_connected = False

    def get_texture(self):
        """
        Get latest camera frame texture (thread-safe).

        Returns:
            wgpu.GPUTexture at runtime.width x runtime.height

        If camera disconnected, returns black texture instead of crashing.
        """
        with self._texture_lock:
            if self._camera_connected and self._latest_texture is not None:
                return self._latest_texture
            else:
                # Camera disconnected or no frames yet - return black
                return self._create_black_texture()

    def stop(self):
        """Stop camera capture and cleanup."""
        self.session.stopRunning()

        with self._texture_lock:
            if self._latest_texture is not None:
                self._latest_texture.destroy()
                self._latest_texture = None

            if self._black_texture is not None:
                self._black_texture.destroy()
                self._black_texture = None
