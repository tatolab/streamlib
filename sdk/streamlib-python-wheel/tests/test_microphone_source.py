# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.MicrophoneSource` — the audio built-in, marker to numpy view.

The marker tests are pure Python. The graph tests boot a real engine, which
initializes a GPU context, so they carry `requires_gpu` like every other graph
test here.

Deliberately arm-agnostic: the backend chain picks whichever arm the machine
running this actually has, and these assertions hold on all of them — the last
arm needs no audio library at all, so this still runs in a container. What is
*not* asserted here is that the timestamps are the device's own rather than the
moment of publication; that needs a real device to be provable at all, and it
lives in `runtime/streamlib-engine/tests/`
`pipewire_arm_stamps_blocks_with_the_devices_own_timing.rs`.
"""

import math

import json
import re
from pathlib import Path

import pytest

import streamlib
from microphone_source_named_device_app import UNOPENABLE_DEVICE_ID

MICROPHONE_SOURCE_APP = Path(__file__).parent / "microphone_source_app.py"
NAMED_DEVICE_APP = Path(__file__).parent / "microphone_source_named_device_app.py"

BLOCKS_SEEN = re.compile(r"MARKER:BLOCKS_SEEN (\[.*\])")

# How far a block's stamp may sit from where its sample count puts it. A device
# arm's clock is the reference, not the nominal rate, so a few hundred
# nanoseconds a block is the device rather than a defect; a lost block is a
# whole quantum, which this is nowhere near.
DEVICE_CLOCK_TOLERANCE_NS_PER_BLOCK = 100_000


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        streamlib.MicrophoneSource()


def test_display_name_defaults_to_the_type_name():
    runtime = streamlib.Runtime()
    try:
        microphone = runtime.add(streamlib.MicrophoneSource)
        assert microphone.display_name == "MicrophoneSource"
    finally:
        runtime.shutdown()


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_the_microphone_publishes_blocks_a_python_processor_reads_as_numpy(
    start_app_under_test,
):
    """The whole audio path, end to end: marker class → native registration →
    the probed backend capturing in the app process → an `AudioBlock` bag read
    as a numpy view by a Python processor in its own helper process.

    Added with no `config`, so this is also the added-without-config proof: the
    config travels to the engine as JSON and every field of a built-in's config
    struct carries a serde default, so `{}` deserializes and `null` does not.
    """
    app = start_app_under_test(MICROPHONE_SOURCE_APP)
    app.await_output_containing("MARKER:BLOCKS_SEEN", "the probe's first blocks")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    match = BLOCKS_SEEN.search(app.output)
    assert match is not None, f"no parseable block report:\n{app.output}"
    readings = json.loads(match.group(1))
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
    assert "dropped at the device edge" not in app.output, (
        f"the source dropped blocks while the probe was reporting:\n{app.output}"
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
    samples_between_first_and_last = sum(
        reading["sample_count"] for reading in readings[:-1]
    )
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
    start_app_under_test,
):
    """A machine with no audio is a supported environment; a wrong device id is
    a wiring error. Landing on a different device would be worse than failing,
    so the source refuses and the processor never reaches Running."""
    app = start_app_under_test(NAMED_DEVICE_APP)
    app.await_output_containing(
        "MARKER:NOT_EVERY_PROCESSOR_RUNNING", "the readiness wait to report Error"
    )
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    assert "MARKER:EVERY_PROCESSOR_RUNNING" not in app.output
    assert UNOPENABLE_DEVICE_ID in app.output, (
        f"the refusal must name the device that was asked for:\n{app.output}"
    )
