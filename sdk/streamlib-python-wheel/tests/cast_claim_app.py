# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One cast-claim probe against a real source, in its real placement.

Run as a real `python app.py`: the probe executes in its own helper process and
reports what it saw over the child→parent log forwarding.

The source is the second argument. The camera is what the lifetime probes need
— only a real capture pool recycles a slot underneath a held frame. The native
test pattern serves the probes that only need a real surface published by a
real producer, so they run on any GPU rather than only on a rig with a camera.
"""

import os
import sys

import streamlib

import cast_claim_probes


def _source(runtime: "streamlib.Runtime", source_name: str):
    if source_name == "camera":
        return runtime.add(
            streamlib.CameraSource,
            config={
                "device_id": os.environ.get("STREAMLIB_CAMERA_DEVICE", "/dev/video0")
            },
        )
    if source_name == "test_pattern":
        return runtime.add(
            streamlib.TestPatternSource, config={"width": 640, "height": 480}
        )
    raise SystemExit(f"unknown source {source_name!r}: use 'camera' or 'test_pattern'")


def main(probe_class_name: str, source_name: str) -> None:
    runtime = streamlib.Runtime()
    source = _source(runtime, source_name)
    probe = runtime.add(getattr(cast_claim_probes, probe_class_name))
    runtime.connect(source.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
