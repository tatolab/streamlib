#!/usr/bin/env python3
"""Test script to check AVFoundation buffer dimensions"""

import objc
from Foundation import NSObject
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
    CVPixelBufferGetWidth,
    CVPixelBufferGetHeight,
    CVPixelBufferGetBytesPerRow,
)
from CoreMedia import CMSampleBufferGetImageBuffer
import ctypes

class TestDelegate(NSObject):
    def init(self):
        self = objc.super(TestDelegate, self).init()
        if self is None:
            return None
        self.frame_count = 0
        return self

    def captureOutput_didOutputSampleBuffer_fromConnection_(self, output, sample_buffer, connection):
        if self.frame_count == 0:
            pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer)
            if pixel_buffer:
                width = CVPixelBufferGetWidth(pixel_buffer)
                height = CVPixelBufferGetHeight(pixel_buffer)
                bytes_per_row = CVPixelBufferGetBytesPerRow(pixel_buffer)

                print(f"Frame {self.frame_count + 1}:")
                print(f"  Width: {width}")
                print(f"  Height: {height}")
                print(f"  Bytes per row: {bytes_per_row}")
                print(f"  Expected (width*4): {width * 4}")
                print(f"  Padding: {bytes_per_row - (width * 4)} bytes")
                print(f"  Stride width: {bytes_per_row // 4}")

        self.frame_count += 1
        if self.frame_count >= 3:
            print(f"\nCaptured {self.frame_count} frames, exiting...")
            import os
            os._exit(0)

# Find FaceTime camera
devices = AVCaptureDevice.devicesWithMediaType_(AVMediaTypeVideo)
device = None
for d in devices:
    if "FaceTime HD Camera" in d.localizedName():
        device = d
        break

if not device:
    print("FaceTime HD Camera not found!")
    exit(1)

print(f"Using: {device.localizedName()}")

# Create session
session = AVCaptureSession.alloc().init()
session.setSessionPreset_(AVCaptureSessionPreset1920x1080)

# Add input
device_input, error = AVCaptureDeviceInput.deviceInputWithDevice_error_(device, None)
if error:
    print(f"Error creating input: {error}")
    exit(1)

session.addInput_(device_input)

# Create output
output = AVCaptureVideoDataOutput.alloc().init()
settings = {kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA}
output.setVideoSettings_(settings)

# Create delegate
delegate = TestDelegate.alloc().init()

# Create dispatch queue
libdispatch = ctypes.CDLL('/usr/lib/system/libdispatch.dylib')
dispatch_get_global_queue = libdispatch.dispatch_get_global_queue
dispatch_get_global_queue.restype = ctypes.c_void_p
dispatch_get_global_queue.argtypes = [ctypes.c_long, ctypes.c_ulong]
queue_ptr = dispatch_get_global_queue(0, 0)
queue = objc.objc_object(c_void_p=queue_ptr)

output.setSampleBufferDelegate_queue_(delegate, queue)
session.addOutput_(output)

# Start session and run loop
from Foundation import NSRunLoop, NSDefaultRunLoopMode, NSDate
import threading

def capture_thread():
    session.startRunning()
    print("Session started, waiting for frames...")
    run_loop = NSRunLoop.currentRunLoop()
    while session.isRunning():
        run_loop.runMode_beforeDate_(NSDefaultRunLoopMode, NSDate.dateWithTimeIntervalSinceNow_(0.1))

thread = threading.Thread(target=capture_thread, daemon=False)
thread.start()
thread.join(timeout=10)
