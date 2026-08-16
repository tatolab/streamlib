# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast-claim contract, over a live camera, in the real placement.

`[surface-id-lifetime-contract]` says the claim on a frame's pixels is taken at
the typed cast — the moment a consumer names what it is holding — and released
when that object drops; a consumer that reads the bag as a dict takes none and
rides pool depth instead. Both halves are load-bearing: the first is what lets
a processor outlive the producer's ring, and the second is what keeps the
strictness dial optional rather than compulsory.

Proving it needs all the real parts at once — a producer whose pool actually
cycles, a consumer one process away, and the surface-share service between
them — so these drive `cast_claim_app.py` out of process against `/dev/video0`
and assert on the probe's own report. The A/B is the whole design: the two
probes differ in one line, `into=VideoFrame` versus nothing, and must reach
opposite outcomes. Either one passing alone proves little.

Camera-gated, and rig-only like every `requires_gpu` test here — CI runs none
of them.
"""

import json
import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "cast_claim_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_claim_probe(start_app_under_test, probe_class_name: str) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker."""
    if not Path("/dev/video0").exists():
        pytest.skip("no camera on this rig")
    app = start_app_under_test(APP, probe_class_name)
    app.await_output_containing("MARKER:PROBE_RESULT", f"{probe_class_name}'s result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def _require_a_moving_scene(observation: dict) -> None:
    """A camera pointed at a still wall cannot fail either probe: unchanged
    pixels are what a held frame is supposed to show, and also what an
    overwritten slot shows when nothing moved."""
    if not observation["the_source_produced_a_different_picture"]:
        pytest.skip(
            "every frame this camera produced was identical, so a frame staying "
            "still proves nothing — point the rig at a scene that moves"
        )


def test_a_typed_cast_claims_its_frame_and_outlives_the_producers_ring(
    start_app_under_test,
):
    """`read(port, into=VideoFrame)` takes the claim, and holding the object is
    what keeps the producer off that slot.

    No handle, no view, no context manager — the frame alone. A lease that rode
    any of those instead would release at `close()` and let the camera recycle
    the slot underneath a consumer that is still holding the frame.
    """
    observation = run_claim_probe(start_app_under_test, "TypedCastHoldsItsFrameProbe")
    _require_a_moving_scene(observation)

    assert observation["claim_taken"] is True, (
        "the read must offer the cast a claim; a frame that took none is "
        "protected by pool depth alone"
    )
    assert not observation["late_read_refused_as_recycled"], (
        "the held frame's id was refused as recycled after the producer ran "
        f"{observation['frames_the_producer_ran_ahead']} frames past it: "
        f"{observation['late_read_refusal']}"
    )
    assert observation["held_frame_unchanged"], (
        "the pixels under the held surface id changed while the producer ran "
        f"{observation['frames_the_producer_ran_ahead']} frames past it — the "
        "claim did not keep its pool slot"
    )


def test_an_untyped_read_claims_nothing_and_is_refused_once_the_slot_cycles(
    start_app_under_test,
):
    """The control, and the half that keeps the dial optional.

    A bag read as a dict claims nothing by design — a consumer may want its own
    frame type, or none. What the contract owes that consumer is not protection
    it did not ask for, but a *loud* failure: outwaiting pool depth is an
    error naming the recycling, never somebody else's pixels served silently.
    """
    observation = run_claim_probe(start_app_under_test, "UntypedReadHoldsNothingProbe")
    _require_a_moving_scene(observation)

    assert observation["claim_taken"] is False, (
        "an untyped read must take no claim; taking one would make the "
        "strictness dial compulsory"
    )
    assert observation["late_read_refused_as_recycled"], (
        "the recycled id resolved instead of being refused — a stale id "
        "serving the slot's newer pixels is the silent wrongness this "
        "contract exists to make impossible"
    )
    assert "recycled frame" in (observation["late_read_refusal"] or ""), (
        "the refusal must name the recycling so a reader learns the rule at "
        f"the point of failure, got: {observation['late_read_refusal']!r}"
    )
