# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Graphs a `ShaderEffect` test runs as a real `python` app.

Launched with this example's directory on `PYTHONPATH`, so the app process and
every helper import `processors.shader_effect` under the same name the showcase
does.
"""

import json
import sys

from shader_effect_test_processors import (
    KnownPatternPixelBufferSource,
    RenderedPixelReportingSink,
)
from streamlib import Runtime

from processors.shader_effect import SHIPPED_SHADERS_DIRECTORY, ShaderEffect

FRAGMENT_GLSL_THAT_DOES_NOT_COMPILE = """\
#version 450
layout(set = 0, binding = 0) uniform sampler2D upstream_frame;
layout(location = 0) out vec4 painted_colour;
void main() {
    painted_colour = no_such_function(upstream_frame);
}
"""


def _shipped_fragment_glsl(shader_file_name: str) -> str:
    return (SHIPPED_SHADERS_DIRECTORY / shader_file_name).read_text(encoding="utf-8")


def scenario_one_shipped_look(
    shader_file_name: str, pixel_coordinates_to_report: "list[list[int]]"
) -> None:
    """Known pattern → the shipped look → the reporting sink."""
    runtime = Runtime()
    source = runtime.add(KnownPatternPixelBufferSource)
    effect = runtime.add(
        ShaderEffect,
        config={"fragment_glsl": _shipped_fragment_glsl(shader_file_name)},
    )
    sink = runtime.add(
        RenderedPixelReportingSink,
        config={"pixel_coordinates_to_report": pixel_coordinates_to_report},
    )
    runtime.connect(
        source.output("video_to_downstream"), effect.input("video_from_upstream")
    )
    runtime.connect(
        effect.output("video_to_downstream"), sink.input("video_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_a_look_that_does_not_compile_beside_one_that_does() -> None:
    """The pattern fanned to a grayscale chain that reports and to an effect
    whose shader cannot compile."""
    runtime = Runtime()
    source = runtime.add(KnownPatternPixelBufferSource)
    working_effect = runtime.add(
        ShaderEffect,
        config={"fragment_glsl": _shipped_fragment_glsl("grayscale.frag")},
    )
    sink = runtime.add(
        RenderedPixelReportingSink,
        config={"pixel_coordinates_to_report": [[0, 0]]},
    )
    effect_that_does_not_compile = runtime.add(
        ShaderEffect,
        config={"fragment_glsl": FRAGMENT_GLSL_THAT_DOES_NOT_COMPILE},
    )
    runtime.connect(
        source.output("video_to_downstream"),
        working_effect.input("video_from_upstream"),
    )
    runtime.connect(
        working_effect.output("video_to_downstream"), sink.input("video_from_upstream")
    )
    runtime.connect(
        source.output("video_to_downstream"),
        effect_that_does_not_compile.input("video_from_upstream"),
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "one_shipped_look":
        scenario_one_shipped_look(sys.argv[2], json.loads(sys.argv[3]))
    elif sys.argv[1] == "a_look_that_does_not_compile_beside_one_that_does":
        scenario_a_look_that_does_not_compile_beside_one_that_does()
    else:
        raise SystemExit(f"no scenario named {sys.argv[1]!r}")
