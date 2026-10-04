# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one `GlslPixelEffect` probe in
its real placement, a helper process."""

import sys
from typing import Any

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream

import glsl_pixel_effect_probes


def _wire_a_test_pattern_into_probe(
    stream: Stream, probe_class: type, probe_config: dict[str, Any]
) -> None:
    pattern = stream.add(
        streamlib.TestPatternSource,
        config={
            "width": glsl_pixel_effect_probes.FRAME_WIDTH,
            "height": glsl_pixel_effect_probes.FRAME_HEIGHT,
        },
    )
    probe = stream.add(probe_class, config=probe_config)
    stream.connect(pattern.output("video"), probe.input("video_from_upstream"))


@stream
def a_test_pattern_into_an_inverting_effect_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.InvertingEffectProbe, {"effect": "invert"}
    )


@stream
def a_test_pattern_into_an_identity_effect_probe(stream: Stream) -> None:
    """The invert check's negative control: the same probe, running an identity effect."""
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.InvertingEffectProbe, {"effect": "identity"}
    )


@stream
def a_test_pattern_into_the_pre_declared_helpers_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.PreDeclaredHelpersProbe, {}
    )


@stream
def a_test_pattern_into_the_every_dial_type_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.EveryDialTypeProbe, {}
    )


@stream
def a_test_pattern_into_the_compiler_diagnostic_line_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.CompilerDiagnosticLineProbe, {}
    )


@stream
def a_test_pattern_into_the_copy_refused_frame_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(
        stream, glsl_pixel_effect_probes.CopyRefusedFrameProbe, {}
    )


STREAM_BY_SCENARIO = {
    "invert": a_test_pattern_into_an_inverting_effect_probe,
    "invert_negative_control": a_test_pattern_into_an_identity_effect_probe,
    "PreDeclaredHelpersProbe": a_test_pattern_into_the_pre_declared_helpers_probe,
    "EveryDialTypeProbe": a_test_pattern_into_the_every_dial_type_probe,
    "CompilerDiagnosticLineProbe": a_test_pattern_into_the_compiler_diagnostic_line_probe,
    "CopyRefusedFrameProbe": a_test_pattern_into_the_copy_refused_frame_probe,
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[sys.argv[1]])
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
