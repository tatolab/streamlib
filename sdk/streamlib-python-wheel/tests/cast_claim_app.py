# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One cast-claim probe against the live camera, in its real placement.

Run as a real `python app.py`: the probe executes in its own helper process,
holds a delivered frame while the camera's pool recycles underneath it, and
reports what it saw over the child→parent log forwarding.
"""

import os
import sys

import streamlib

import cast_claim_probes


def main(probe_class_name: str) -> None:
    runtime = streamlib.Runtime()
    camera = runtime.add(
        streamlib.CameraSource,
        config={"device_id": os.environ.get("STREAMLIB_CAMERA_DEVICE", "/dev/video0")},
    )
    probe = runtime.add(getattr(cast_claim_probes, probe_class_name))
    runtime.connect(camera.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main(sys.argv[1])
