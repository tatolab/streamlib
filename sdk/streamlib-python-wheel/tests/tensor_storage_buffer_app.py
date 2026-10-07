# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run a Python tensor producer and its consumer in their real
placement: two helper processes, the surface id crossing between them."""

import sys
from typing import Any

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tensor_storage_buffer_probes import (
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    MINIMUM_INTERVAL_BETWEEN_HELD_TENSOR_PUBLISHES_NS,
    POOL_ROTATION_DEPTH,
    HeldTensorRereadingSink,
    PublishedTensorReadingSink,
    TensorStorageBufferKernelBindingProbe,
    TensorStorageBufferPublishingSource,
)


def _wire_the_publishing_source_into(
    stream_builder: StreamBuilder,
    source_config: dict[str, Any],
    sink_class: type,
    sink_config: dict[str, Any] | None = None,
) -> None:
    source = stream_builder.add(TensorStorageBufferPublishingSource, config=source_config)
    sink = stream_builder.add(sink_class, config=sink_config)
    stream_builder.connect(
        source.output("tensors_to_downstream"), sink.input("tensors_from_upstream")
    )


@stream
def a_written_tensor_is_read_by_another_process(stream_builder: StreamBuilder) -> None:
    """Exactly the pool's depth of tensors, so no slot is republished while
    the consumer may still be resolving it."""
    _wire_the_publishing_source_into(
        stream_builder,
        {"tensor_name": "model_input", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "model_input"},
    )


@stream
def an_odd_shaped_tensor_round_trips(stream_builder: StreamBuilder) -> None:
    _wire_the_publishing_source_into(
        stream_builder,
        {"tensor_name": "odd", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "odd"},
    )


@stream
def a_reader_resolving_a_different_id_sees_different_values(stream_builder: StreamBuilder) -> None:
    _wire_the_publishing_source_into(
        stream_builder,
        {"tensor_name": "model_input", "frames_to_publish": POOL_ROTATION_DEPTH},
        PublishedTensorReadingSink,
        {"tensor_name": "model_input", "resolve_the_previous_frames_id": True},
    )


@stream
def a_held_tensor_is_never_rewritten(stream_builder: StreamBuilder) -> None:
    _wire_the_publishing_source_into(
        stream_builder,
        {
            "tensor_name": "model_input",
            "frames_to_publish": FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
            "minimum_interval_between_publishes_ns": (
                MINIMUM_INTERVAL_BETWEEN_HELD_TENSOR_PUBLISHES_NS
            ),
        },
        HeldTensorRereadingSink,
    )


@stream
def a_tensor_acquired_after_a_window_opens_round_trips(stream_builder: StreamBuilder) -> None:
    """The DEVICE_LOCAL OPAQUE_FD allocation after a swapchain exists — the
    order NVIDIA once answered with a fake out-of-memory."""
    _wire_the_publishing_source_into(
        stream_builder,
        {
            "tensor_name": "model_input",
            "frames_to_publish": POOL_ROTATION_DEPTH,
            "open_a_window_before_acquiring": True,
        },
        PublishedTensorReadingSink,
        {"tensor_name": "model_input"},
    )


@stream
def a_kernel_binds_a_tensor_by_surface_id(stream_builder: StreamBuilder) -> None:
    """One helper, no link: the probe acquires its tensors and reports from
    `setup`."""
    stream_builder.add(TensorStorageBufferKernelBindingProbe)


STREAM_BY_SCENARIO = {
    "a_written_tensor_is_read_by_another_process": (
        a_written_tensor_is_read_by_another_process
    ),
    "an_odd_shaped_tensor_round_trips": an_odd_shaped_tensor_round_trips,
    "a_reader_resolving_a_different_id_sees_different_values": (
        a_reader_resolving_a_different_id_sees_different_values
    ),
    "a_held_tensor_is_never_rewritten": a_held_tensor_is_never_rewritten,
    "a_tensor_acquired_after_a_window_opens_round_trips": (
        a_tensor_acquired_after_a_window_opens_round_trips
    ),
    "a_kernel_binds_a_tensor_by_surface_id": a_kernel_binds_a_tensor_by_surface_id,
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[sys.argv[1]])
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
