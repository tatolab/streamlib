# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run a Python tensor producer and its consumer in their real
placement: two helper processes, the surface id crossing between them."""

import sys

import streamlib
from tensor_storage_buffer_probes import (
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    MINIMUM_INTERVAL_BETWEEN_HELD_TENSOR_PUBLISHES_NS,
    POOL_ROTATION_DEPTH,
    HeldTensorRereadingSink,
    PublishedTensorReadingSink,
    TensorStorageBufferKernelBindingProbe,
    TensorStorageBufferPublishingSource,
)


def _run(source_config: dict, sink_class, sink_config: "dict | None" = None) -> None:
    runtime = streamlib.Runtime()
    source = runtime.add(TensorStorageBufferPublishingSource, config=source_config)
    sink = (
        runtime.add(sink_class, config=sink_config)
        if sink_config is not None
        else runtime.add(sink_class)
    )
    runtime.connect(
        source.output("tensors_to_downstream"), sink.input("tensors_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_a_written_tensor_is_read_by_another_process() -> None:
    """Exactly the pool's depth of tensors, so no slot is republished while
    the consumer may still be resolving it."""
    _run(
        {"tensor_name": "model_input", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "model_input"},
    )


def scenario_an_odd_shaped_tensor_round_trips() -> None:
    _run(
        {"tensor_name": "odd", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "odd"},
    )


def scenario_a_reader_resolving_a_different_id_sees_different_values() -> None:
    _run(
        {"tensor_name": "model_input", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "model_input", "resolve_the_previous_frames_id": True},
    )


def scenario_a_held_tensor_is_never_rewritten() -> None:
    _run(
        {
            "tensor_name": "model_input",
            "frames_to_publish": FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
            "minimum_interval_between_publishes_ns": (
                MINIMUM_INTERVAL_BETWEEN_HELD_TENSOR_PUBLISHES_NS
            ),
        },
        HeldTensorRereadingSink,
    )


def scenario_a_tensor_acquired_after_a_window_opens_round_trips() -> None:
    """The DEVICE_LOCAL OPAQUE_FD allocation after a swapchain exists — the
    order NVIDIA once answered with a fake out-of-memory."""
    _run(
        {
            "tensor_name": "model_input",
            "frames_to_publish": POOL_ROTATION_DEPTH,
            "open_a_window_before_acquiring": True,
        },
        PublishedTensorReadingSink,
        {"tensor_name": "model_input"},
    )


def scenario_a_kernel_binds_a_tensor_by_surface_id() -> None:
    """One helper, no link: the probe acquires its tensors and reports from
    `setup`."""
    runtime = streamlib.Runtime()
    runtime.add(TensorStorageBufferKernelBindingProbe)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


SCENARIOS = {
    "a_written_tensor_is_read_by_another_process": (
        scenario_a_written_tensor_is_read_by_another_process
    ),
    "an_odd_shaped_tensor_round_trips": scenario_an_odd_shaped_tensor_round_trips,
    "a_reader_resolving_a_different_id_sees_different_values": (
        scenario_a_reader_resolving_a_different_id_sees_different_values
    ),
    "a_held_tensor_is_never_rewritten": scenario_a_held_tensor_is_never_rewritten,
    "a_tensor_acquired_after_a_window_opens_round_trips": (
        scenario_a_tensor_acquired_after_a_window_opens_round_trips
    ),
    "a_kernel_binds_a_tensor_by_surface_id": (
        scenario_a_kernel_binds_a_tensor_by_surface_id
    ),
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
