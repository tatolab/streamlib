#!/usr/bin/env python3
"""List available cameras on this system"""

import AVFoundation

def list_cameras():
    """List all available video capture devices"""
    # Get all video devices
    devices = AVFoundation.AVCaptureDevice.devicesWithMediaType_(
        AVFoundation.AVMediaTypeVideo
    )

    print("\nAvailable Cameras:")
    print("=" * 60)

    if not devices or len(devices) == 0:
        print("No cameras found!")
        return

    for i, device in enumerate(devices):
        print(f"\n{i+1}. {device.localizedName()}")
        print(f"   Unique ID: {device.uniqueID()}")
        print(f"   Model ID: {device.modelID()}")
        print(f"   Connected: {device.isConnected()}")

        # Get supported formats
        formats = device.formats()
        if formats:
            print(f"   Supported formats: {len(formats)}")
            # Show a few sample formats
            for fmt in list(formats)[:3]:
                desc = fmt.formatDescription()
                dims = AVFoundation.CMVideoFormatDescriptionGetDimensions(desc)
                print(f"     - {dims.width}x{dims.height}")

    print("\n" + "=" * 60)
    print(f"\nTotal cameras found: {len(devices)}")

if __name__ == '__main__':
    list_cameras()
