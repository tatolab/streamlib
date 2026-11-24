# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Texture member `MakeBundle` holds at the read keeps its pixels on `tatolabd`: one picture
wired into the member, its link then quiet while a level makes a bundle every tick and the
ring the picture came from keeps turning over. Every bundle hands on the one held picture,
never a slot the ring rewrote, and the ring keeps producing.
"""

from __future__ import annotations

from collections.abc import Callable

import pytest

from conftest import TatolabdUnderTest
from held_texture_member_nodes import BUNDLES_TO_READ, HELD_PICTURE_VALUE
from held_texture_member_streams import (
    one_picture_held_in_a_member_while_its_ring_turns_over,
)
from runtime_process_under_test import NOT_A_MARKER_LINE, MarkerLineParser

pytestmark = pytest.mark.requires_gpu

PICTURES_READ_TIMEOUT_SECONDS = 180.0


def ring_reports_written_so_far(tatolabd: TatolabdUnderTest) -> int:
    """How many `RING_TURNED_OVER` reports standard error holds, so the next one awaited is
    written after the pictures were read."""
    parser = MarkerLineParser("RING_TURNED_OVER")
    return sum(
        parser.payload_of(line) is not NOT_A_MARKER_LINE
        for line in list(tatolabd.stderr_lines)
    )


def test_a_held_picture_keeps_its_pixels_while_its_ring_turns_over(
    start_tatolabd_running_stream: Callable[..., TatolabdUnderTest],
):
    """Each of `BUNDLES_TO_READ` pictures taken off the bundle is the one picture written,
    every pixel `HELD_PICTURE_VALUE`, under one surface id. The ring never rewrites its slot,
    keeps turning over to the end of the read without refusing a frame, and turns over
    through the same set of other slots throughout: holding the picture grows the ring by
    nothing past the one slot it pins.

    Fail-without-fix: the held picture claims nothing, so the ring rewrites its slot with the
    churn value and the pictures taken off later bundles read the churn's pixels.
    """
    tatolabd = start_tatolabd_running_stream(
        one_picture_held_in_a_member_while_its_ring_turns_over
    )
    first_turned_over = tatolabd.await_marker(
        "RING_TURNED_OVER", timeout=PICTURES_READ_TIMEOUT_SECONDS
    )
    read = tatolabd.await_marker(
        "HELD_PICTURE_READ", timeout=PICTURES_READ_TIMEOUT_SECONDS
    )
    turned_over = tatolabd.await_marker(
        "RING_TURNED_OVER",
        timeout=PICTURES_READ_TIMEOUT_SECONDS,
        occurrence=ring_reports_written_so_far(tatolabd) + 1,
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert read["pictures_read"] >= BUNDLES_TO_READ, read
    assert read["pictures_not_held"] == 0, read
    assert read["values_seen"] == [HELD_PICTURE_VALUE], read
    assert read["surface_ids"] == [turned_over["held_picture_surface_id"]], (
        read,
        turned_over,
    )
    held_slot = turned_over["held_picture_surface_id"].split("#")[0]
    assert held_slot not in turned_over["churn_slots"], turned_over
    assert (
        turned_over["churn_frames_written"] > first_turned_over["churn_frames_written"]
    ), (
        first_turned_over,
        turned_over,
    )
    assert turned_over["acquires_refused"] == 0, turned_over
    assert turned_over["churn_slots"] == first_turned_over["churn_slots"], (
        first_turned_over,
        turned_over,
    )
