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

pytestmark = pytest.mark.requires_gpu

PICTURES_READ_TIMEOUT_SECONDS = 180.0

#: Slots the ring may turn over through for its one `newest` link besides the held picture:
#: its depth of 2, the link's mailbox and the default borrow quota, with headroom. A ring that
#: grew past it would be pinning more than the one held slot.
MOST_CHURN_SLOTS = 8


def test_a_held_picture_keeps_its_pixels_while_its_ring_turns_over(
    start_tatolabd_running_stream: Callable[..., TatolabdUnderTest],
):
    """Each of `BUNDLES_TO_READ` pictures taken off the bundle is the one picture written,
    every pixel `HELD_PICTURE_VALUE`, under one surface id; the ring never rewrites its slot,
    never refuses a frame, and turns over through a bounded set of other slots.

    Fail-without-fix: the held picture claims nothing, so the ring rewrites its slot with the
    churn value and the pictures taken off later bundles read the churn's pixels.
    """
    tatolabd = start_tatolabd_running_stream(
        one_picture_held_in_a_member_while_its_ring_turns_over
    )
    read = tatolabd.await_marker(
        "HELD_PICTURE_READ", timeout=PICTURES_READ_TIMEOUT_SECONDS
    )
    turned_over = tatolabd.await_marker(
        "RING_TURNED_OVER", timeout=PICTURES_READ_TIMEOUT_SECONDS
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
    assert turned_over["acquires_refused"] == 0, turned_over
    assert len(turned_over["churn_slots"]) <= MOST_CHURN_SLOTS, turned_over
