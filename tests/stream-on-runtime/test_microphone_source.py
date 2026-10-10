# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.MicrophoneSource` — the audio built-in, built-in class to numpy view.

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
from microphone_source_streams import (
    UNOPENABLE_DEVICE_ID,
    microphone_into_an_audio_block_probe,
    microphone_source_naming_an_unopenable_device,
    one_microphone_source_left_unnamed,
)
from tatolab.stream import compile_stream_to_graph

READINESS_TIMEOUT_SECONDS = 10.0
MICROPHONE_NODE_NAME = "microphonesource"

# How far a block's stamp may sit from where its sample count puts it. A device
# arm's clock is the reference, not the nominal rate, so a few hundred
# nanoseconds a block is the device rather than a defect; a lost block is a
# whole quantum, which this is nowhere near.
DEVICE_CLOCK_TOLERANCE_NS_PER_BLOCK = 100_000


# ---- built-in class semantics (no GPU) -------------------------------------


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = compile_stream_to_graph(one_microphone_source_left_unnamed)
    assert [node["name"] for node in graph["nodes"]] == [MICROPHONE_NODE_NAME]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_the_microphone_publishes_blocks_a_python_processor_reads_as_numpy(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The whole audio path, end to end: built-in class → native registration →
    the probed backend capturing in `tatolabd` → an `AudioBlock` bag read as a
    numpy view by a Python processor in its own processor interpreter.

    `stream_builder.add` with no `config` records `{}`, so this is also the
    added-without-config proof: every field of a built-in's config struct
    carries a serde default, so `{}` deserializes.
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
        assert reading["shape"] == [reading["sample_count"], reading["channels"]]
        assert reading["numpy_type"] == "<f4", (
            "the wire is little-endian by contract, so the cast must not take "
            "the platform-native spelling"
        )
        assert reading["samples_are_a_view_over_the_bag_bytes"], (
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
    assert "dropped at the device edge" not in tatolabd.stderr_text, (
        f"the source dropped blocks while the probe was reporting:\n{tatolabd.recent_stderr()}"
    )

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
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Landing on a different device would be worse than failing,
    so the source refuses and the processor never reaches Running."""
    tatolabd = start_tatolabd_running_stream(microphone_source_naming_an_unopenable_device)
    stream_name = tatolabd.await_the_latest_attached_stream_loaded()
    node_states = tatolabd.local_api_client().await_every_node_past_setup(
        stream=stream_name, timeout=READINESS_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert node_states[MICROPHONE_NODE_NAME] == "Error", (
        f"the microphone must refuse at setup rather than reach Running: {node_states}"
    )
    assert UNOPENABLE_DEVICE_ID in tatolabd.stderr_text, (
        f"the refusal must name the device that was asked for:\n{tatolabd.recent_stderr()}"
    )
