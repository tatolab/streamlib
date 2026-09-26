# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one surface-copy probe in its
real placement, a helper process."""

import sys

import streamlib

import surface_copy_probes


def scenario(probe_name: str, config: dict) -> None:
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource,
        config={
            "width": surface_copy_probes.FRAME_WIDTH,
            "height": surface_copy_probes.FRAME_HEIGHT,
        },
    )
    probe = runtime.add(getattr(surface_copy_probes, probe_name), config=config)
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "frame_landing":
        scenario("FrameLandingProbe", {})
    elif sys.argv[1] == "frame_landing_negative_control":
        scenario("FrameLandingProbe", {"skip_copy": True})
    else:
        scenario(sys.argv[1], {})
