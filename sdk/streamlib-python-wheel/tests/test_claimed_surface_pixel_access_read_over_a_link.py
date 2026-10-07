# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ctx.inputs.read(port, into=T)` for a type that composes
`ClaimedSurfacePixelAccess`, over a real link.

The composable's own half, with the capability stood in for, is the stream
suite's (`sdk/tatolab-stream/tests/test_claimed_surface_pixel_access.py`).
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.stream import ClaimedSurfacePixelAccess

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

FRAME_BAG = {
    "surface_id": "surface-7",
    "width_in_pixels": 1280,
    "height_in_pixels": 720,
}

OUTPUT_PORT = "frames_to_downstream"
INPUT_PORT = "frames_from_upstream"


def claim_taken_on(
    cast_object: ClaimedSurfacePixelAccess, surface_id_field: str = "surface_id"
) -> Any:
    """The lease a cast object took for one of its declared surfaces."""
    return cast_object.pixel_access_to_the_surface_declared_in(
        surface_id_field
    )._check_out_lease_on_the_claimed_surface


@dataclass(frozen=True, init=False)
class DepthFrame(ClaimedSurfacePixelAccess):
    """A user-authored cast type: declare the fields, inherit the constructor,
    get the protocol."""

    surface_id: str
    width_in_pixels: int
    height_in_pixels: int
    units_per_metre: float = 1.0
    tags: list[str] = field(default_factory=list)


# ---- the spelling itself, over a real link ---------------------------------


def test_a_composing_type_read_over_a_link_arrives_built_from_the_bag():
    """`ctx.inputs.read(port, into=T)` end to end for a type the wheel never
    heard of: a bag crosses real iceoryx2 ports and comes back as the composing
    object with its declared fields set.

    This context is built without an escalate bridge, so its GPU capability
    reaches nothing and the claim is refused — which leaves an ordinary object
    rather than an exception at the read, exactly as an unreachable GPU must.
    """
    unique = f"composable{os.getpid()}"
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
    source.write_to_output_port(
        OUTPUT_PORT, {**FRAME_BAG, "a_key_a_future_producer_adds": "ignored"}
    )

    frame = ctx.inputs.read(INPUT_PORT, into=DepthFrame)

    assert frame is not None, "the wired input received nothing"
    assert frame == DepthFrame(**FRAME_BAG)
    assert claim_taken_on(frame) is None, (
        "a capability that reaches nothing claims nothing"
    )
    with pytest.raises(RuntimeError, match="not reachable"):
        frame.__dlpack__()
