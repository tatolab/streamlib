# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one `ModelInputTensorKernel`
probe in its real placement, a helper process."""

import sys
from typing import Any

import tatolab.runtime
import tatolab.stream
from tatolab.stream import Stream, compile_stream_to_graph, stream

import model_input_tensor_kernel_probes


def _probe_class_name_and_config_named_on_the_command_line() -> tuple[
    str, dict[str, Any] | None
]:
    if sys.argv[1] == "matrix":
        return "ModelInputTensorMatrixProbe", {"fit_case_name": sys.argv[2]}
    if sys.argv[1] == "matrix_with_the_wrong_mean":
        return "ModelInputTensorMatrixProbe", {
            "fit_case_name": sys.argv[2],
            "compile_with_the_wrong_mean": True,
        }
    return sys.argv[1], None


@stream
def a_test_pattern_into_one_model_input_tensor_kernel_probe(stream: Stream) -> None:
    pattern = stream.add(
        tatolab.stream.TestPatternSource,
        config={
            "width": model_input_tensor_kernel_probes.FRAME_WIDTH,
            "height": model_input_tensor_kernel_probes.FRAME_HEIGHT,
        },
    )
    probe_class_name, probe_config = (
        _probe_class_name_and_config_named_on_the_command_line()
    )
    probe = stream.add(
        getattr(model_input_tensor_kernel_probes, probe_class_name),
        config=probe_config,
    )
    stream.connect(pattern.output("video"), probe.input("video_from_upstream"))


if __name__ == "__main__":
    graph = compile_stream_to_graph(
        a_test_pattern_into_one_model_input_tensor_kernel_probe
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
