# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor in its own processor interpreter producing texture-backed frames.

`NodeOutputTextureRing` is the surface a Python source publishes frames
from, and its unit tests stand the capability in — they own the slot
bookkeeping and cannot reach the engine that allocates. What is proven here is
the half a stand-in cannot: the engine hands back slots that stay registered
for the producer's whole life, rotate as the ring says they do, and carry
pixels a *different* process can resolve by surface id and read.

Each scenario runs the producer in a processor interpreter beneath `tatolabd`
and asserts on the `MARKER:PROBE_RESULT` lines it forwards to `tatolabd`'s
stderr.
"""

from collections.abc import Callable

import pytest

import texture_ring_producer_streams
from runtime_process_under_test import RuntimeProcessUnderTest
from texture_ring_producer_probes import (
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    FRAME_HEIGHT,
    FRAME_WIDTH,
    RING_DEPTH,
    pixel_value_of_frame,
)

pytestmark = pytest.mark.requires_gpu


def run_scenario(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]", scenario: str, awaited_reports: int
) -> dict:
    """Run one scenario to completion, and return its reports by probe name."""
    tatolabd = start_tatolabd(texture_ring_producer_streams.STREAM_BY_SCENARIO[scenario])
    for report_number in range(awaited_reports):
        report = tatolabd.await_marker("PROBE_RESULT", occurrence=report_number + 1)
        if isinstance(report, dict) and "failure" in report:
            break
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    reports_by_probe: "dict[str, list[dict]]" = {}
    for report in tatolabd.marker_payloads("PROBE_RESULT"):
        assert isinstance(report, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
        if "failure" in report:
            pytest.fail(
                f"{report['probe']} raised in its processor interpreter:\n{report['failure']}"
            )
        reports_by_probe.setdefault(report["probe"], []).append(report)
    return reports_by_probe


def slot_of(surface_id: str) -> str:
    """The pool slot a published `<slot>#<generation>` id names."""
    return surface_id.rsplit("#", 1)[0]


def test_a_python_source_publishes_frames_from_the_slots_its_ring_rotates(
    start_tatolabd,
):
    """Depth-many distinct slots, and the frame past the end reuses the first
    slot under a new frame id.

    Against a live engine rather than a stand-in, which is what makes the
    reuse meaningful: a pool that minted a fresh texture per acquire would
    publish from three distinct slots here.
    """
    # The sink reports once per frame it reads; the producer once at its quota.
    reports = run_scenario(start_tatolabd, "ring_rotation", (RING_DEPTH + 1) + 1)
    published = reports["TextureRingPublishingVideoSource"][0][
        "surface_ids_published"
    ]

    assert len(published) == RING_DEPTH + 1
    assert len(set(published)) == RING_DEPTH + 1, (
        f"every publish names its own frame: {published}"
    )
    slots = [slot_of(surface_id) for surface_id in published]
    assert len(set(slots)) == RING_DEPTH, (
        f"a ring {RING_DEPTH} deep published from {len(set(slots))} distinct "
        f"slots: {published}"
    )
    assert slots[RING_DEPTH] == slots[0], (
        f"the frame past the ring's depth published from {published[RING_DEPTH]!r} "
        f"rather than wrapping onto {published[0]!r}'s slot"
    )


def test_a_frame_a_consumer_holds_keeps_its_pixels_while_the_producer_produces(
    start_tatolabd,
):
    """The consumer claims the first frame with a typed read and re-reads it as
    the producer publishes several ring depths past it: the pixels are still
    frame 0's, and the held slot is never published from again."""
    later_frames = FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD
    reports = run_scenario(
        start_tatolabd,
        "a_claimed_frame_holds_still",
        later_frames + 1,  # one report per later frame, plus the producer's
    )
    published = reports["TextureRingPublishingVideoSource"][0][
        "surface_ids_published"
    ]
    rereads = reports["ClaimedFrameHoldingSink"]

    assert len(rereads) == later_frames
    for reread in rereads:
        assert reread["held_surface_id"] == published[0]
        assert reread["held_top_left_pixel"] == [pixel_value_of_frame(0)] * 4, (
            f"after {reread['later_frames_seen']} later frames the held frame read "
            f"{reread['held_top_left_pixel']} rather than frame 0's pixels"
        )
    assert all(
        slot_of(surface_id) != slot_of(published[0]) for surface_id in published[1:]
    ), f"the held frame's slot was published from again: {published}"


def test_the_same_schedule_with_no_claim_recycles_the_first_frame(
    start_tatolabd,
):
    """The negative control: nothing holds the first frame, so the producer
    republishes its slot, and the old id is refused as recycled — never read
    back as a newer frame's pixels."""
    later_frames = FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD
    reports = run_scenario(
        start_tatolabd,
        "an_unclaimed_frame_is_recycled",
        later_frames + 1,
    )
    published = reports["TextureRingPublishingVideoSource"][0][
        "surface_ids_published"
    ]
    attempts = reports["UnclaimedFrameHoldingSink"]

    assert any(
        slot_of(surface_id) == slot_of(published[0]) for surface_id in published[1:]
    ), f"with no claim the first frame's slot was never republished: {published}"
    refusals = [attempt["refusal"] for attempt in attempts if "refusal" in attempt]
    assert refusals, f"the recycled first frame still resolved: {attempts}"
    assert all("recycl" in refusal for refusal in refusals), refusals
    for attempt in attempts:
        if "held_top_left_pixel" in attempt:
            assert attempt["held_top_left_pixel"] == [pixel_value_of_frame(0)] * 4


def test_the_pixels_a_python_source_writes_are_read_by_another_process(
    start_tatolabd,
):
    """The producer writes in a processor interpreter of its own; the consumer resolves
    the published id in a second one and sees those bytes.

    That the resolve succeeds at all is half the assertion: the slot is
    registered because the ring still holds it. A producer that let its
    texture go at the end of `process()` would unregister the id one line
    after publishing it, and this would fail on the refusal rather than on
    the pixels.
    """
    reports = run_scenario(
        start_tatolabd,
        "published_frames_reach_a_downstream_consumer",
        RING_DEPTH + 1,  # one report per frame read, plus the producer's
    )
    published = reports["TextureRingPublishingVideoSource"][0][
        "surface_ids_published"
    ]
    frames_read = reports["PublishedFramePixelReadingSink"]

    assert len(frames_read) == RING_DEPTH
    assert [frame["surface_id"] for frame in frames_read] == published
    for frame in frames_read:
        expected = pixel_value_of_frame(frame["frame_index"])
        assert frame["extent"] == [FRAME_WIDTH, FRAME_HEIGHT]
        assert frame["top_left_pixel"] == [expected] * 4, (
            f"frame {frame['frame_index']} read back "
            f"{frame['top_left_pixel']} rather than the {expected} its producer wrote"
        )


def test_the_producer_and_its_consumer_run_in_different_processes(
    start_tatolabd,
):
    """The premise the two assertions above rest on — one Python processor per
    processor interpreter, so the surface id really did cross a boundary."""
    reports = run_scenario(
        start_tatolabd,
        "published_frames_reach_a_downstream_consumer",
        RING_DEPTH + 1,  # one report per frame read, plus the producer's
    )
    producer_pid = reports["TextureRingPublishingVideoSource"][0]["pid"]
    consumer_pids = {frame["pid"] for frame in reports["PublishedFramePixelReadingSink"]}

    assert len(consumer_pids) == 1, f"the consumer reported from {consumer_pids}"
    assert producer_pid not in consumer_pids
