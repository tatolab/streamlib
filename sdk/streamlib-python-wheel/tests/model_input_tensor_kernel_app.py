# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one `ModelInputTensorKernel`
probe in its real placement, a helper process."""

import sys

import streamlib

import model_input_tensor_kernel_probes


def scenario(probe_name: str, config: "dict | None" = None) -> None:
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource,
        config={
            "width": model_input_tensor_kernel_probes.FRAME_WIDTH,
            "height": model_input_tensor_kernel_probes.FRAME_HEIGHT,
        },
    )
    probe_class = getattr(model_input_tensor_kernel_probes, probe_name)
    probe = (
        runtime.add(probe_class, config=config)
        if config is not None
        else runtime.add(probe_class)
    )
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "matrix":
        scenario("ModelInputTensorMatrixProbe", {"fit_case_name": sys.argv[2]})
    elif sys.argv[1] == "matrix_with_the_wrong_mean":
        scenario(
            "ModelInputTensorMatrixProbe",
            {"fit_case_name": sys.argv[2], "compile_with_the_wrong_mean": True},
        )
    else:
        scenario(sys.argv[1])
