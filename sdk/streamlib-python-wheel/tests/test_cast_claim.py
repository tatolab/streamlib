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

`[cast-object-tensor-protocol]` then says that same object *is* the tensor
protocol: `torch.from_dlpack(frame)` straight off the read, GPU-resident, with
the resolve and the lock absorbed. That half is proven here for a cast type the
wheel never heard of as well as for `VideoFrame` — a protocol that only worked
for the shipped class would be exactly the privilege the plan says it must not
have. Its write doors close the gradient, and the only place they can be proven
is one with a real producer behind the frame: an edit through `writable()` or
`cpu()` is checked by resolving the surface again from scratch, because
published means every *other* holder observes it.

Every test here is `requires_gpu`, so CI runs none of them, but they do not all
need the same hardware. The lifetime pair, the device-side tensor pair and the
camera write-door pair are camera-gated; the rest drive the engine's native
test pattern, and the host-side ones consume with plain numpy, so they need a
GPU and nothing else. Each gate skips by name, so a missing camera or a
CPU-only torch reads as what it is rather than as a failure of the capability.

The write doors are proven against both sources deliberately. A producer that
published an internal transient under the frame's id would leave the
test-pattern cases passing while every camera edit went somewhere no in-process
consumer looks — which is exactly the defect #1932 fixed, and exactly what the
camera pair now guards.
"""

import json
import os
import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "cast_claim_app.py"

# The same default `cast_claim_app.py` opens, read from the same place: a rig
# pointing the app at another node must not be gated on /dev/video0.
CAMERA_DEVICE = os.environ.get("STREAMLIB_CAMERA_DEVICE", "/dev/video0")

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_claim_probe(
    start_app_under_test, probe_class_name: str, source: str = "camera"
) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker."""
    if source == "camera" and not Path(CAMERA_DEVICE).exists():
        pytest.skip(f"no camera at {CAMERA_DEVICE} on this rig")
    app = start_app_under_test(APP, probe_class_name, source)
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


# ---- the bare tensor protocol, against a real published surface ------------


def _assert_the_bare_view_is_this_frames_pixels(observation: dict) -> None:
    """What every host-side bare-protocol probe must show, whichever type read
    the bag. The seam is entirely real here — a resolved handle, a read-only
    lock and a capsule the engine minted; only the consumer is plain numpy."""
    assert observation["claim_taken"] is True, (
        "the view rides the claim, so a frame that took none has no stable "
        "pixels to export"
    )
    assert observation["device_the_object_advertised"][0] == 2, (
        "the bare read path is GPU-resident: the object must advertise kDLCUDA "
        "(2), and a host answer here means the device export silently "
        "downgraded"
    )
    assert observation["pixels_are_not_all_zero"], (
        "an all-zero surface would make the comparison below vacuous"
    )
    assert (
        observation["host_view_shape"]
        == observation["host_view_shape_through_the_resolve_and_lock"]
    )
    assert observation["the_bare_view_is_the_same_pixels"], (
        "the bare view must be this frame's pixels, not merely a valid view "
        "over some surface"
    )


def test_a_user_authored_cast_type_reaches_its_pixels_with_no_ceremony(
    start_app_under_test,
):
    """The no-privilege claim, against a real surface: a type the wheel never
    heard of composes the shipped piece and hands its pixels to a DLPack
    consumer.

    No `resolve_surface`, no `lock`, no context manager anywhere in the
    probe — the object the read handed back is the tensor-protocol producer.
    """
    observation = run_claim_probe(
        start_app_under_test,
        "AUserAuthoredCastReachesItsPixelsBareProbe",
        source="test_pattern",
    )

    _assert_the_bare_view_is_this_frames_pixels(observation)


def test_the_shipped_video_frame_reaches_its_pixels_the_same_way(
    start_app_under_test,
):
    """The parity half: `VideoFrame` is built from the same composable, so it
    must reach its pixels through the same path with the same result. A
    difference here is a privilege the plan does not grant it."""
    observation = run_claim_probe(
        start_app_under_test,
        "TheShippedVideoFrameReachesItsPixelsBareProbe",
        source="test_pattern",
    )

    _assert_the_bare_view_is_this_frames_pixels(observation)


# ---- the device half: a CUDA package eating the bare capsule ---------------


def _assert_the_bare_cuda_tensor_is_this_frames_pixels(observation: dict) -> None:
    assert observation["claim_taken"] is True
    assert observation["tensor_device"].startswith("cuda"), (
        "`torch.from_dlpack(frame)` must land on the GPU — a host tensor here "
        "means the device export silently downgraded"
    )
    assert (
        observation["tensor_shape"]
        == observation["tensor_shape_through_the_resolve_and_lock"]
    )
    assert (
        observation["checksum_through_the_bare_view"]
        == observation["checksum_through_the_resolve_and_lock"]
    )


def _require_a_cuda_consumer() -> None:
    """These need a CUDA-built consumer in the venv, which the CPU wheel CI
    installs and a rig does not necessarily. Skipped rather than failed: what
    is missing is the consumer, not the capability under test."""
    torch = pytest.importorskip("torch")
    if not torch.cuda.is_available():
        pytest.skip("torch in this venv is not CUDA-built, so it cannot eat a "
                    "kDLCUDA capsule")


def test_a_user_authored_cast_type_reaches_torch_as_a_cuda_tensor(
    start_app_under_test,
):
    """`torch.from_dlpack(frame)` off a live camera frame, in a real helper
    placement — the shortest spelling is the fast path, and it is GPU-resident.
    """
    _require_a_cuda_consumer()
    observation = run_claim_probe(
        start_app_under_test, "AUserAuthoredCastReachesItsPixelsAsACudaTensorProbe"
    )

    _assert_the_bare_cuda_tensor_is_this_frames_pixels(observation)


def test_the_shipped_video_frame_reaches_torch_as_a_cuda_tensor(
    start_app_under_test,
):
    _require_a_cuda_consumer()
    observation = run_claim_probe(
        start_app_under_test, "TheShippedVideoFrameReachesItsPixelsAsACudaTensorProbe"
    )

    _assert_the_bare_cuda_tensor_is_this_frames_pixels(observation)


# ---- the write doors: an edit through the object, seen on the surface -------


def _assert_the_edit_reached_the_surface(observation: dict) -> None:
    assert observation["claim_taken"] is True
    assert observation["the_frame_did_not_already_carry_the_edit"], (
        "the producer happened to send exactly the edit, so the surface "
        "carrying it afterwards proves nothing"
    )
    assert observation["the_edited_rows_carry_the_edit"], (
        "a fresh resolve of the same surface does not show the edit, so the "
        "block edge published nothing"
    )
    assert observation["the_rest_of_the_frame_is_untouched"], (
        "the frame outside the edited rows changed too, so what landed was an "
        "overwrite rather than an edit of the frame that was read"
    )


def test_a_gpu_edit_through_the_write_door_is_on_the_surface_after_the_block(
    start_app_under_test,
):
    """`with frame.writable() as t:` over a live frame, in a real helper placement:
    a CUDA package edits in place and the surface carries the edit once the
    block ends, which is what every other holder observes.

    Sourced from the native test pattern rather than the camera deliberately —
    see the refusal test below, which is why a camera frame cannot take this
    door at all."""
    _require_a_cuda_consumer()
    observation = run_claim_probe(
        start_app_under_test,
        "TheGpuWriteDoorEditsTheFrameProbe",
        source="test_pattern",
    )

    _assert_the_edit_reached_the_surface(observation)


def test_a_raise_inside_the_gpu_write_door_leaves_the_frame_the_engine_held(
    start_app_under_test,
):
    """The other half of the one write rule. A half-written view blitted back
    would publish a torn frame that surfaces as corruption somewhere downstream
    rather than at the `raise`, so the write is dropped — and the exception
    still reaches the caller."""
    _require_a_cuda_consumer()
    observation = run_claim_probe(
        start_app_under_test,
        "ARaiseInsideTheGpuWriteDoorDiscardsTheEditProbe",
        source="test_pattern",
    )

    assert observation["claim_taken"] is True
    assert observation["the_exception_propagated"], (
        "the scope suppressed the raise, which no write scope may do"
    )
    assert observation["the_surface_still_holds_the_frame_the_producer_sent"]


def test_a_cpu_edit_through_the_write_door_is_on_the_surface_after_the_block(
    start_app_under_test,
):
    """The named slow path against a real surface, with plain numpy as the
    consumer: `with frame.cpu() as img:` reaches the host mapping and the edit
    is the surface's own afterwards."""
    observation = run_claim_probe(
        start_app_under_test,
        "TheCpuWriteDoorEditsTheFrameProbe",
        source="test_pattern",
    )

    _assert_the_edit_reached_the_surface(observation)


def test_a_raise_inside_the_cpu_write_door_propagates_and_closes_the_scope(
    start_app_under_test,
):
    """What the CPU door can honestly promise on its exception path.

    The host view is the surface's own mapping, so bytes already written are
    already in the frame — there is no staging to drop, and this asserts no
    discard. What it does assert is the half that is real: the raise is never
    suppressed, and the scope closes cleanly enough that the surface is still
    reachable afterwards.
    """
    observation = run_claim_probe(
        start_app_under_test,
        "ARaiseInsideTheCpuWriteDoorPropagatesProbe",
        source="test_pattern",
    )

    assert observation["claim_taken"] is True
    assert observation["the_exception_propagated"]
    assert observation["the_door_opens_again_after_a_raise"]


def test_a_gpu_edit_of_a_camera_frame_is_on_the_surface_after_the_block(
    start_app_under_test,
):
    """The same GPU door against a live camera frame.

    A camera publishes one picture under one id — its capture ring is its own
    scratch space and answers to nothing outside it — so a camera frame takes
    an edit exactly like any other single-backed frame, and a fresh resolve
    shows it. Locked separately from the test-pattern case because the camera
    is the source the product's first-run app actually uses, and because a
    producer that leaked an internal texture under the published id would
    make this refuse while the test-pattern case kept passing.
    """
    _require_a_cuda_consumer()
    observation = run_claim_probe(
        start_app_under_test, "TheGpuWriteDoorEditsTheFrameProbe"
    )

    _assert_the_edit_reached_the_surface(observation)


def test_a_cpu_edit_of_a_camera_frame_is_on_the_surface_after_the_block(
    start_app_under_test,
):
    """The CPU door's half of the same claim, with plain numpy as the
    consumer and no CUDA anywhere in it."""
    observation = run_claim_probe(
        start_app_under_test, "TheCpuWriteDoorEditsTheFrameProbe"
    )

    _assert_the_edit_reached_the_surface(observation)
