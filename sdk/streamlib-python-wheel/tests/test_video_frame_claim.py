# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ctx.inputs.read(port, into=VideoFrame)` over a real link, offering the
frame the capability the read holds.

The frame's own half — that it claims when something offers the means, holds
the claim, and releases it by going away — is the stream suite's
(`sdk/tatolab-stream/tests/test_video_frame_claim.py`), with the capability
stood in for.
"""

from __future__ import annotations

import os
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.stream import (
    ClaimedSurfacePixelAccess,
    ColorInfo,
    VideoFrame,
    gpu_limited_access_of_the_typed_read_in_progress,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

FRAME_BAG = {
    "surface_id": "surface-7",
    "width": 1280,
    "height": 720,
    "timestamp_ns": 123_456_789,
}

OUTPUT_PORT = "frames_to_downstream"
INPUT_PORT = "frames_from_upstream"


def claim_taken_on(frame: ClaimedSurfacePixelAccess) -> Any:
    """The lease a frame took on the surface it names."""
    return frame.pixel_access_to_the_surface_declared_in(
        "surface_id"
    )._check_out_lease_on_the_claimed_surface


# ---- the spelling itself, over a real link ---------------------------------


class FrameThatRecordsWhatTheReadOffered:
    """A frame class the wheel does not ship, written the way anyone could —
    it keeps whatever the read put on offer, so a test can name it."""

    def __init__(self, surface_id: str, **rest_of_the_bag: object) -> None:
        self.surface_id = surface_id
        self.offered = gpu_limited_access_of_the_typed_read_in_progress()


def test_a_frame_read_over_a_link_arrives_cast_and_survives_an_unreachable_gpu():
    """`ctx.inputs.read(port, into=VideoFrame)` end to end: a bag crosses real
    iceoryx2 ports and arrives as a frame with its metadata cast.

    This context is built without an escalate bridge, so its GPU capability
    reaches nothing — which is the case that matters here. The read offers that
    capability anyway (asserted, so unwiring the offer fails this test), the
    frame tries to claim, and the refusal leaves an ordinary frame rather than
    an exception at the read. What the claim does when the route *is* live
    needs a surface-share service, and is proven in the wheel's Rust tests.
    """
    unique = f"framecast{os.getpid()}"
    channel_service_name = f"{unique}/frames"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    # The destination subscribes first: iceoryx2 drops a send with no
    # subscriber attached. Both planes live on this thread — its ports are
    # `!Send`.
    destination = _engine.NodeLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, channel_service_name,
        notify_service_name,
        "read_next_in_order", 8, 8, 2, 1, link_id,
    )  # fmt: skip
    source = _engine.NodeLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, link_id,
    )  # fmt: skip

    ctx = _engine.open_runtime_context_full_access_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )
    bag_from_upstream = {
        **FRAME_BAG,
        "fps": 30,
        "color_info": {"primaries": "bt709"},
        # A producer's own key this cast does not read. The bag is an open map,
        # and the day one is added must not be the day typed reads start
        # raising — which would take the frame's protection with it.
        "a_key_a_future_producer_adds": "ignored",
    }
    source.write_to_output_port(OUTPUT_PORT, bag_from_upstream)

    frame = ctx.inputs.read(INPUT_PORT, into=VideoFrame)

    assert frame is not None, "the wired input received nothing"
    assert frame.surface_id == "surface-7"
    assert frame.color_info == ColorInfo(primaries="bt709")
    assert claim_taken_on(frame) is None, (
        "a capability that reaches nothing claims nothing"
    )

    # And that None is a refusal the frame swallowed, not an offer that never
    # happened. Read the same bag into a class that keeps what it was offered:
    # this fails if the read stops offering, whatever the shipped type does.
    source.write_to_output_port(OUTPUT_PORT, bag_from_upstream)
    recording = ctx.inputs.read(INPUT_PORT, into=FrameThatRecordsWhatTheReadOffered)
    assert recording is not None
    offered = recording.offered
    assert offered is ctx.gpu_limited_access, (
        "the read must offer the constructing type this processor's own capability"
    )
    assert offered is not None
    with pytest.raises(RuntimeError, match="not reachable"):
        offered.claim_surface_against_producer_reuse("surface-7")
