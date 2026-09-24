# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Which camera a camera-gated test drives, on either floor.

One answer for the app that opens the camera and the test that gates on it:
`STREAMLIB_CAMERA_DEVICE` when set, `/dev/video0` on Linux, and on macOS the
first built-in camera — what `CameraSource` opens with no `device_id`.
"""

import os
import subprocess
import sys
from pathlib import Path

CAMERA_DEVICE_ENVIRONMENT_VARIABLE = "STREAMLIB_CAMERA_DEVICE"
LINUX_DEFAULT_CAMERA_DEVICE = "/dev/video0"


def camera_source_config() -> dict:
    """The `CameraSource` config that opens this rig's camera."""
    named = os.environ.get(CAMERA_DEVICE_ENVIRONMENT_VARIABLE)
    if named:
        return {"device_id": named}
    if sys.platform == "darwin":
        return {}
    return {"device_id": LINUX_DEFAULT_CAMERA_DEVICE}


def reason_this_rig_has_no_camera() -> "str | None":
    """Why the camera tests cannot run here, or None when a camera is present."""
    named = os.environ.get(CAMERA_DEVICE_ENVIRONMENT_VARIABLE)
    if sys.platform == "darwin":
        if named:
            return None
        cameras = subprocess.run(
            ["system_profiler", "SPCameraDataType"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
        if "Unique ID" not in cameras:
            return "no camera on this Mac"
        return None
    device = named or LINUX_DEFAULT_CAMERA_DEVICE
    if not Path(device).exists():
        return f"no camera at {device} on this rig"
    return None
