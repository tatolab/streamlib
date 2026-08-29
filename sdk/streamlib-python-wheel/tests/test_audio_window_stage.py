# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The window contract reaching a helper-placed Python consumer.

The declaration surface is #2032's; this is the stage delivering on it in the
placement every Python processor actually runs in. The contract crosses the
parent→child wiring envelope beside `read_mode`, and the child's own
`InputMailboxesInner` — the same Rust the parent's mailboxes run — resamples,
mixes down and frames before `process()` ever sees a bag.

The graph tests boot a real engine, which initializes a GPU context, so they
carry `requires_gpu` like every other graph test here. The wiring-refusal tests
below need `/dev/shm` and nothing else, so they run in CI.
"""

import json
import re
from pathlib import Path

import pytest

from streamlib import ProcessorLinkDataAccess

APP = Path(__file__).parent / "audio_window_app.py"

WINDOWS_SEEN = re.compile(r"MARKER:WINDOWS_SEEN (\[.*\])")
ROLLING_WINDOWS_SEEN = re.compile(r"MARKER:ROLLING_WINDOWS_SEEN (\[.*\])")

# The contract both probes declare, and what it makes each window worth.
CONTRACT_SAMPLE_RATE = 16_000
CONTRACT_WINDOW_SIZE = 512
CONTIGUOUS_HOP_NS = 32_000_000
ROLLING_HOP_NS = 10_000_000


def run_until(start_app_under_test, scenario: str, awaited: str, what: str):
    app = start_app_under_test(APP, scenario)
    app.await_output_containing(awaited, what)
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    return app


def readings_from(app, pattern, what: str):
    match = pattern.search(app.output)
    assert match is not None, f"no parseable {what} report:\n{app.output}"
    return json.loads(match.group(1))


def assert_every_window_matches_the_contract(readings):
    for reading in readings:
        assert reading["sample_count"] == CONTRACT_WINDOW_SIZE, (
            "the contract's whole promise is an exact-size block; a short or long "
            f"one means the stage handed over a partial window: {reading}"
        )
        assert reading["sample_rate"] == CONTRACT_SAMPLE_RATE
        assert reading["channels"] == 1
        assert reading["dtype"] == "f32"
        assert reading["shape"] == [CONTRACT_WINDOW_SIZE, 1]


@pytest.mark.requires_gpu
def test_a_helper_placed_consumer_reads_exact_windows_at_the_rate_it_declared(
    start_app_under_test,
):
    """A device capturing at its own rate reaches a 16 kHz mono 512/512 port as
    exactly-512-sample windows 32 ms apart, in a child process.

    The rate the machine's device settles on is whatever it settles on; what the
    contract promises is that the consumer never sees it.
    """
    app = run_until(
        start_app_under_test,
        "contiguous_windows",
        "MARKER:WINDOWS_SEEN",
        "the probe's first windows",
    )
    readings = readings_from(app, WINDOWS_SEEN, "window")
    assert len(readings) >= 2, "the cadence assertion needs two windows to subtract"
    assert_every_window_matches_the_contract(readings)

    # Checked before the arithmetic: a discontinuity flush is a legitimate
    # outcome and re-anchors the run, so it should fail by name rather than as a
    # confusing subtraction.
    assert "flushed rather than emitting a window that spans the gap" not in app.output, (
        f"the stage flushed while the probe was reporting:\n{app.output}"
    )
    assert "dropped at the device edge" not in app.output, (
        f"the source dropped blocks while the probe was reporting:\n{app.output}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [CONTIGUOUS_HOP_NS] * len(steps), (
        "512 samples at 16 kHz is exactly 32 ms, and every stamp derives from one "
        f"anchor within a contiguous run — got {steps}"
    )


@pytest.mark.requires_gpu
def test_a_hop_below_the_window_rolls_at_the_hops_cadence_not_the_windows(
    start_app_under_test,
):
    """A rolling window is still exact-size; only its cadence changes."""
    app = run_until(
        start_app_under_test,
        "rolling_windows",
        "MARKER:ROLLING_WINDOWS_SEEN",
        "the probe's first rolling windows",
    )
    readings = readings_from(app, ROLLING_WINDOWS_SEEN, "rolling window")
    assert len(readings) >= 2
    assert_every_window_matches_the_contract(readings)

    assert "flushed rather than emitting a window that spans the gap" not in app.output, (
        f"the stage flushed while the probe was reporting:\n{app.output}"
    )

    stamps = [reading["first_sample_timestamp_ns"] for reading in readings]
    steps = [later - earlier for earlier, later in zip(stamps, stamps[1:])]
    assert steps == [ROLLING_HOP_NS] * len(steps), (
        "a hop of 160 at 16 kHz advances by exactly 10 ms while each window still "
        f"carries 512 samples — got {steps}"
    )


# ---- the child's own reading of the envelope (no device, no GPU) ------------


def a_helper_process_data_plane() -> ProcessorLinkDataAccess:
    """The object a child builds for itself, with its own iceoryx2 node."""
    return ProcessorLinkDataAccess()


def wire_with_window(data_plane: ProcessorLinkDataAccess, audio_window) -> None:
    data_plane.wire_input_link(
        "audio",
        "streamlib/tests/audio-window/never-opened",
        "streamlib/tests/audio-window/never-notified",
        "read_next_in_order",
        16,
        2,
        1,
        "L-test",
        audio_window,
    )


def test_a_child_refuses_a_window_contract_whose_field_it_cannot_read():
    """The parent sends the five values; a key it got wrong is named here
    rather than surfacing as an anonymous decode failure."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(data_plane, {"sample_rate": 16_000, "channels": 1})

    rendered = str(refusal.value)
    assert "audio" in rendered and "dtype" in rendered, (
        f"the refusal must name the port and the field; got {rendered}"
    )


def test_a_child_refuses_a_window_contract_the_stage_could_not_honour():
    """The same validator both languages' declaration paths call, applied again
    where the child receives the contract: a hop above the window would silently
    discard the samples between windows."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 1_024,
            },
        )

    rendered = str(refusal.value)
    assert "512" in rendered and "1024" in rendered, (
        f"the refusal must name both numbers; got {rendered}"
    )
