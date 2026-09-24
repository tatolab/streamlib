# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A helper-placed Python processor producing texture-backed frames.

`ProcessorOutputTextureRing` is the surface a Python source publishes frames
from, and its unit tests stand the capability in — they own the slot
bookkeeping and cannot reach the engine that allocates. What is proven here is
the half a stand-in cannot: the engine hands back slots that stay registered
for the producer's whole life, rotate as the ring says they do, and carry
pixels a *different* process can resolve by surface id and read.

Both scenarios run the producer out of process and assert on the
`MARKER:PROBE_RESULT` lines its helper forwards.
"""

import json
import re
from pathlib import Path

import pytest

from texture_ring_producer_probes import (
    FRAME_HEIGHT,
    FRAME_WIDTH,
    RING_DEPTH,
    pixel_value_of_frame,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "texture_ring_producer_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_scenario(start_app_under_test, scenario: str, awaited_reports: int) -> dict:
    """Run one scenario to completion, and return its reports by probe name."""
    app = start_app_under_test(APP, scenario)
    for report_number in range(awaited_reports):
        app.await_output_containing(
            "MARKER:PROBE_RESULT", f"report {report_number + 1} of {awaited_reports}"
        )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    reports_by_probe: "dict[str, list[dict]]" = {}
    for match in PROBE_RESULT.finditer(app.output):
        report = json.loads(match.group(1))
        if "failure" in report:
            pytest.fail(
                f"{report['probe']} raised in its helper process:\n{report['failure']}"
            )
        reports_by_probe.setdefault(report["probe"], []).append(report)
    return reports_by_probe


def test_a_python_source_publishes_frames_from_the_slots_its_ring_rotates(
    start_app_under_test,
):
    """Depth-many distinct slots, and the frame past the end reuses the first.

    Against a live engine rather than a stand-in, which is what makes the
    reuse meaningful: a pool that minted a fresh texture per acquire would
    pass the unit test's bookkeeping and fail here with three distinct ids.
    """
    # The sink reports once per frame it reads; the producer once at its quota.
    reports = run_scenario(start_app_under_test, "ring_rotation", (RING_DEPTH + 1) + 1)
    published = reports["TextureRingPublishingVideoSource"][0][
        "surface_ids_published"
    ]

    assert len(published) == RING_DEPTH + 1
    assert len(set(published)) == RING_DEPTH, (
        f"a ring {RING_DEPTH} deep published {len(set(published))} distinct "
        f"surfaces: {published}"
    )
    assert published[RING_DEPTH] == published[0], (
        f"the frame past the ring's depth published from {published[RING_DEPTH]!r} "
        f"rather than wrapping onto {published[0]!r}"
    )


def test_the_pixels_a_python_source_writes_are_read_by_another_process(
    start_app_under_test,
):
    """The producer writes in its own child interpreter; the consumer resolves
    the published id in a second one and sees those bytes.

    That the resolve succeeds at all is half the assertion: the slot is
    registered because the ring still holds it. A producer that let its
    texture go at the end of `process()` would unregister the id one line
    after publishing it, and this would fail on the refusal rather than on
    the pixels.
    """
    reports = run_scenario(
        start_app_under_test,
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
    start_app_under_test,
):
    """The premise the two assertions above rest on — one Python processor per
    helper process, so the surface id really did cross a boundary."""
    reports = run_scenario(
        start_app_under_test,
        "published_frames_reach_a_downstream_consumer",
        RING_DEPTH + 1,  # one report per frame read, plus the producer's
    )
    producer_pid = reports["TextureRingPublishingVideoSource"][0]["pid"]
    consumer_pids = {frame["pid"] for frame in reports["PublishedFramePixelReadingSink"]}

    assert len(consumer_pids) == 1, f"the consumer reported from {consumer_pids}"
    assert producer_pid not in consumer_pids
