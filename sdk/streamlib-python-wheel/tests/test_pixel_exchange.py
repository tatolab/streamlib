# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The pixel-exchange surface, proven from a processor's real placement.

Pixels reach Python in place: a numpy view or a DLPack tensor addresses the
same bytes the engine allocated — now one process away, imported over the
surface-share checkout — so a write through the view is visible to anything
else holding that surface. The three contracts worth breaking a build over
are here: the row pitch survives into the strides, an export taken without a
lock is refused, and a tensor that outlives its frame keeps addressing live
memory rather than a recycled pool slot.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line over the child→parent log forwarding; the
tests drive the app out of process and assert on that line.
"""

import json
import re
from pathlib import Path

import pytest

from pixel_exchange_probes import SURFACE_HEIGHT, SURFACE_WIDTH

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "pixel_exchange_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, scenario: str) -> dict:
    """One scenario, one observation dict — or a failure carrying the probe's
    own traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, scenario)
    app.await_output_containing("MARKER:PROBE_RESULT", f"the {scenario} probe's result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


# ---------------------------------------------------------------------------
# Layout
# ---------------------------------------------------------------------------


def test_the_numpy_view_is_a_shared_window_with_the_allocations_row_pitch(
    start_app_under_test,
):
    """`(height, width, 4)` uint8, strides straight off the allocation.

    The strides are the load-bearing part: DLPack counts them in elements and
    numpy in bytes, so a producer that forgot the conversion would hand back an
    array that reads every fourth row as if it were adjacent.
    """
    observation = run_probe(start_app_under_test, "NumpyViewProbe")
    assert observation["shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]
    assert observation["dtype"] == "uint8"
    assert observation["strides"] == [observation["bytes_per_row"], 4, 1]
    assert observation["bytes_per_row"] >= SURFACE_WIDTH * 4
    assert not observation["owns_its_data"], (
        "the view copied the pixels instead of sharing them"
    )


# ---------------------------------------------------------------------------
# The lock gate
# ---------------------------------------------------------------------------


def test_a_read_only_lock_produces_a_read_only_view(start_app_under_test):
    """`read_only` is carried to the consumer, not just recorded locally.

    Only DLPack's versioned exchange shape has a flags field, so this is
    also the check that the version negotiation is wired up: on the legacy
    shape every tensor arrives read-only and the write lock would look
    broken.
    """
    observation = run_probe(start_app_under_test, "LockModeProbe")
    assert "read-only" in observation["write_under_a_read_lock"]
    assert observation["write_under_a_write_lock"] == "succeeded"


def test_an_export_taken_without_a_lock_is_refused(start_app_under_test):
    """The gate is access discipline, not synchronisation.

    It does not wait for anything — ordering against the producer comes from
    publication, since a source finishes its GPU work before it sends the
    frame on. What the gate buys is that read/write intent is stated at the
    call site, which is what carries through to the exported tensor's flags,
    and that `base_address` is None rather than a live pointer nobody
    declared a use for.
    """
    observation = run_probe(start_app_under_test, "UnlockedExportProbe")
    assert "not locked" in observation["unlocked_dlpack"]
    assert observation["unlocked_base_address"] is None
    assert observation["locked_base_address_is_real"]
    assert "not locked" in observation["after_unlock"], (
        "unlock must close the gate again, not leave it open for the surface's life"
    )


# ---------------------------------------------------------------------------
# Sharing — a write through the view is not a write to a copy
# ---------------------------------------------------------------------------


def test_a_write_through_the_view_is_visible_to_another_holder_of_the_surface(
    start_app_under_test,
):
    """Zero-copy means one buffer, not two that agree.

    A second handle resolved by `surface_id` must observe the write, because
    both views address the engine's allocation — here through two independent
    cross-process imports of the same DMA-BUF.
    """
    observation = run_probe(start_app_under_test, "SharedMemoryProbe")
    assert observation["pixel_seen_by_the_reader"] == [11, 22, 33, 44]
    assert observation["an_untouched_pixel"] == [0, 0, 0, 0]


# ---------------------------------------------------------------------------
# Lifetime — the regression the ticket names
# ---------------------------------------------------------------------------


def test_a_tensor_outliving_its_surface_keeps_addressing_live_memory(
    start_app_under_test,
):
    """Use-after-free regression.

    A closed handle must not free the mapping a numpy view still points at.
    The engine value and the pool-slot release live behind a refcount every
    outstanding tensor holds a share of, so `close()` drops the handle's share
    and nothing more.

    Mental-revert: move the release back into `close()` — this test reads freed
    memory, and the failure is a segfault or garbage rather than an assertion.
    """
    observation = run_probe(start_app_under_test, "TensorOutlivesTheSurfaceProbe")
    assert observation["still_readable"] == 7
    assert observation["sum_after_close"] == observation["expected_sum"]


def test_a_held_tensor_is_not_overwritten_by_pool_reuse(start_app_under_test):
    """Ring-slot reuse is gated on the consumer being done.

    Churning the pool past its depth while a tensor is outstanding must not
    hand that tensor's slot to a new acquire — the held pixels stay as written.
    """
    observation = run_probe(start_app_under_test, "PoolCycleProbe")
    assert observation["churn_failures"] == []
    assert observation["held_values"] == [3], (
        f"the held tensor was overwritten by a later acquire: "
        f"saw {observation['held_values']}"
    )


# ---------------------------------------------------------------------------
# Interop
# ---------------------------------------------------------------------------


def test_a_dlpack_consumer_sees_the_same_pixels(start_app_under_test):
    observation = run_probe(start_app_under_test, "DlpackConsumerProbe")
    assert observation["pixel"] == [1, 2, 3, 4]
    assert observation["shape"] == [SURFACE_HEIGHT, SURFACE_WIDTH, 4]


# ---------------------------------------------------------------------------
# The whole point, end to end
# ---------------------------------------------------------------------------


def test_a_processor_edits_a_synthetic_frames_pixels_in_place(start_app_under_test):
    """The user-facing story: source → effect → the pixels really changed.

    A native source produces frames the interpreter never touches; a Python
    processor in its own child reaches into one and rewrites it. Re-resolving
    the surface afterwards is what proves the edit went into the engine's
    memory rather than a copy handed to Python.
    """
    observation = run_probe(start_app_under_test, "inverting_effect")
    assert observation["frame_size"] == [320, 180]
    # The pattern's leftmost SMPTE bar is white, written by the native fill
    # through the engine's own view of the allocation. Reading it back at a
    # known coordinate through the child's independently-derived strides is
    # what catches the two derivations disagreeing — a systematic stride
    # divergence passes every self-consistent assertion below.
    assert observation["before"] == [255, 255, 255, 255], (
        f"pixel (10, 10) should be the white SMPTE bar the native source wrote; "
        f"the child's view read {observation['before']}"
    )
    assert observation["after_through_this_view"] == [
        255 - channel for channel in observation["before"]
    ]
    assert (
        observation["after_through_a_fresh_resolve"]
        == observation["after_through_this_view"]
    ), "a fresh resolve did not see the edit — the pixels Python wrote were a copy"


def test_a_multi_plane_format_is_refused_rather_than_exported_as_luma(
    start_app_under_test,
):
    """NV12 is two planes and DLPack is one buffer.

    Exporting plane 0 would hand back a greyscale image that looks like a
    working colour frame until someone notices the chroma is missing.
    """
    observation = run_probe(start_app_under_test, "UnsupportedFormatProbe")
    assert "one strided linear buffer" in observation["outcome"]
