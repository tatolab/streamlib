# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one `GlslPixelEffect` probe in
its real placement, a helper process."""

import sys

import streamlib

import glsl_pixel_effect_probes


def scenario(probe_name: str, config: dict) -> None:
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource,
        config={
            "width": glsl_pixel_effect_probes.FRAME_WIDTH,
            "height": glsl_pixel_effect_probes.FRAME_HEIGHT,
        },
    )
    probe = runtime.add(getattr(glsl_pixel_effect_probes, probe_name), config=config)
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "invert":
        scenario("InvertingEffectProbe", {"effect": "invert"})
    elif sys.argv[1] == "invert_negative_control":
        scenario("InvertingEffectProbe", {"effect": "identity"})
    else:
        scenario(sys.argv[1], {})
