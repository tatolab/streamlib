# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.microphone.MicrophoneSource` — the capture module, node class to numpy view.

The load test needs no device: `tatolabd` loads the graph and its start is
then refused at the GPU. The graph tests start the engine, which initializes a GPU context,
so they carry `requires_gpu` like every other graph test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. What is
*not* asserted here is that the timestamps are the device's own rather than the
moment of publication; that needs a real device to be provable at all, and it
lives in `runtime/streamlib-engine/tests/`
`pipewire_arm_stamps_blocks_with_the_devices_own_timing.rs` and its CoreAudio sibling.
"""

import math
from collections.abc import Callable

import pytest

from conftest import StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from microphone_device_edge_drops import assert_the_microphone_dropped_no_blocks_at_the_device_edge
from microphone_source_streams import (
    SLOW_MOTION_SCALE,
    UNOPENABLE_DEVICE_ID,
    microphone_into_an_audio_block_probe,
    microphone_into_an_audio_block_probe_in_slow_motion,
    microphone_source_naming_an_unopenable_device,
    one_microphone_source_left_unnamed,
)
from tatolab.stream import run_graph_generator

READINESS_TIMEOUT_SECONDS = 10.0
MICROPHONE_NODE_NAME = "microphonesource"

# How far a block's stamp may sit from where its sample count puts it. A device
# arm's clock is the reference, not the nominal rate, so a few hundred
# nanoseconds a block is the device rather than a defect; a lost block is a
# whole quantum, which this is nowhere near.
DEVICE_CLOCK_TOLERANCE_NS_PER_BLOCK = 100_000


# ---- node class semantics (no GPU) -----------------------------------------


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = run_graph_generator(one_microphone_source_left_unnamed)
    assert [node["name"] for node in graph["nodes"]] == [MICROPHONE_NODE_NAME]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


# ---- the module in a real graph (GPU) --------------------------------------


@pytest.mark.requires_gpu
def test_the_microphone_publishes_blocks_a_python_processor_reads_as_numpy(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The whole audio path, end to end: the module's node class → its native
    half capturing through the probed backend in its helper process → an
    Channels block read as a numpy view by a Python processor in its own
    processor interpreter.

    `stream_builder.add` with no `config` records `{}`, so this is also the
    added-without-config proof: every field of the node's config class has a
    default.
    """
    tatolabd = start_tatolabd_running_stream(microphone_into_an_audio_block_probe)
    readings = tatolabd.await_marker("BLOCKS_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(readings, list), f"no parseable block report:\n{tatolabd.recent_stderr()}"
    assert len(readings) >= 2, "the cadence assertion needs two blocks to subtract"

    for reading in readings:
        assert reading["sample_rate"] > 0
        assert reading["channels"] >= 1
        assert reading["sample_count"] > 0
        assert reading["dtype"] == "f32"
        assert reading["shape"] == [reading["channels"], reading["sample_count"]]
        assert reading["numpy_type"] == "<f4", (
            "the wire is little-endian by contract, so the cast must not take "
            "the platform-native spelling"
        )
        assert reading["samples_borrow_the_sample_with_no_copy"], (
            "reading a block must add no copy of its payload"
        )
        # Not an amplitude assertion — a real capture device may be recording
        # anything, including silence. What this catches is a block framed
        # against the wrong stride or read past its mapping, which surfaces as
        # NaN or a wildly out-of-range scalar rather than as a wrong number.
        assert math.isfinite(reading["loudest_sample"]), (
            f"the cast produced a non-finite sample: {reading}"
        )

    # Checked before the arithmetic below, which a real gap would break: a
    # dropped block is a legitimate outcome of a stalled consumer, and it
    # should fail here by name rather than as a confusing subtraction.
    assert_the_microphone_dropped_no_blocks_at_the_device_edge(tatolabd)

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    assert stamps == sorted(set(stamps)), (
        f"block timestamps are the ordering primitive and must advance: {stamps}"
    )

    # Asserted across the whole span rather than per block: what has to hold is
    # that the elapsed time between the first and last block equals the duration
    # of the samples between them, which is what makes a block's timestamp plus
    # its sample count the next block's expected timestamp.
    #
    # The tolerance is per block and generous next to a quantum. A device arm's
    # stamps come from the device's own clock, which runs a few hundred
    # nanoseconds a block off its nominal rate — real timing, not error. What
    # the bound still catches is the failure that matters: a block dropped or
    # duplicated without being accounted for opens a gap of a whole quantum,
    # two orders of magnitude above this.
    sample_rate = readings[0]["sample_rate"]
    samples_between_first_and_last = sum(reading["sample_count"] for reading in readings[:-1])
    expected_ns = samples_between_first_and_last * 1_000_000_000 // sample_rate
    elapsed_ns = stamps[-1] - stamps[0]
    tolerance_ns = DEVICE_CLOCK_TOLERANCE_NS_PER_BLOCK * len(readings)
    assert abs(elapsed_ns - expected_ns) <= tolerance_ns, (
        f"{samples_between_first_and_last} samples at {sample_rate} Hz should span "
        f"{expected_ns} ns but the timestamps span {elapsed_ns} ns — a block went "
        f"missing, or one claimed an instant it did not cover"
    )


@pytest.mark.requires_gpu
def test_a_slow_motion_stream_stamps_each_block_on_its_stream_clock(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The device still captures in real time, but a block's stamp is the
    stream's time at its first sample: at half speed, the stamps span half the
    duration of the samples between them."""
    tatolabd = start_tatolabd_running_stream(microphone_into_an_audio_block_probe_in_slow_motion)
    readings = tatolabd.await_marker("BLOCKS_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(readings, list), f"no parseable block report:\n{tatolabd.recent_stderr()}"
    assert len(readings) >= 2, "the span assertion needs two blocks to subtract"
    assert_the_microphone_dropped_no_blocks_at_the_device_edge(tatolabd)

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    assert stamps == sorted(set(stamps)), f"block timestamps must advance: {stamps}"

    sample_rate = readings[0]["sample_rate"]
    samples_between_first_and_last = sum(reading["sample_count"] for reading in readings[:-1])
    real_time_span_ns = samples_between_first_and_last * 1_000_000_000 // sample_rate
    expected_ns = int(real_time_span_ns * SLOW_MOTION_SCALE)
    elapsed_ns = stamps[-1] - stamps[0]
    tolerance_ns = DEVICE_CLOCK_TOLERANCE_NS_PER_BLOCK * len(readings)
    assert abs(elapsed_ns - expected_ns) <= tolerance_ns, (
        f"{samples_between_first_and_last} samples at {sample_rate} Hz span {real_time_span_ns} ns "
        f"of real time, so at {SLOW_MOTION_SCALE}x their stamps should span {expected_ns} ns, "
        f"but they span {elapsed_ns} ns"
    )


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Landing on a different device would be worse than failing,
    so the source refuses and the processor never reaches Running."""
    tatolabd = start_tatolabd_running_stream(microphone_source_naming_an_unopenable_device)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    node_states = tatolabd.local_api_client().await_every_node_past_setup(
        stream=stream_id, timeout=READINESS_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert node_states[MICROPHONE_NODE_NAME] == "Error", (
        f"the microphone must refuse at setup rather than reach Running: {node_states}"
    )
    assert UNOPENABLE_DEVICE_ID in tatolabd.stderr_text, (
        f"the refusal must name the device that was asked for:\n{tatolabd.recent_stderr()}"
    )
