# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a `VideoFrame` does with the capability a typed read offers it.

The seam itself — a bag crossing a real link, a real surface-share service,
the lease the pool reads — is proven in the wheel's Rust tests, which are where
a service can be started. What is left to prove here is the frame's own half:
that it claims when something offers the means, holds the claim in a field
nobody has to touch, releases it by going away, and stays an ordinary
construction when nothing is on offer.

The capability is stood in for, because the real one needs a running engine and
this is the GPU-free half of the suite. The stand-in is exactly what the read
offers — an object with `claim_surface_against_producer_reuse` — which is the
point: nothing about the frame is bound to our implementation of it.
"""

from __future__ import annotations

import gc
import weakref

import pytest

from streamlib import ColorInfo, ContentLight, VideoFrame
from streamlib import video_frame as video_frame_module

FRAME_BAG = {
    "surface_id": "surface-7",
    "width": 1280,
    "height": 720,
    "timestamp_ns": 123_456_789,
}

CLAIM_FIELD = "_check_out_lease_on_this_frames_surface"


class ClaimStandingInForALease:
    """What the capability hands back — the frame only has to hold it."""

    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id


class GpuLimitedAccessStandIn:
    """The shape a typed read offers, recording what was claimed."""

    def __init__(self) -> None:
        self.claimed_surface_ids: list[str] = []

    def claim_surface_against_producer_reuse(
        self, surface_id: str
    ) -> ClaimStandingInForALease:
        self.claimed_surface_ids.append(surface_id)
        return ClaimStandingInForALease(surface_id)


class GpuLimitedAccessThatRefuses:
    """A capability whose surface is already gone — the honest race."""

    def claim_surface_against_producer_reuse(self, surface_id: str) -> object:
        raise RuntimeError(
            f"the surface-share service refused check_out of {surface_id!r}: unknown surface"
        )


@pytest.fixture
def offered(monkeypatch: pytest.MonkeyPatch):
    """Offer a capability to every construction in the test, the way a read
    offers one for the length of a construction."""

    def offer(gpu_limited_access: object | None):
        monkeypatch.setattr(
            video_frame_module,
            "gpu_limited_access_of_the_typed_read_in_progress",
            lambda: gpu_limited_access,
        )
        return gpu_limited_access

    return offer


# ---- the claim -------------------------------------------------------------


def test_a_frame_built_under_an_offer_claims_the_surface_it_names(offered):
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert getattr(frame, CLAIM_FIELD).surface_id == "surface-7"


def test_the_claim_goes_away_with_the_frame_and_nothing_is_called(offered):
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)
    claim_still_alive = weakref.ref(getattr(frame, CLAIM_FIELD))
    assert claim_still_alive() is not None

    del frame
    gc.collect()
    assert claim_still_alive() is None, (
        "the frame going out of scope is the whole release protocol"
    )


def test_a_frame_built_from_a_bag_you_are_holding_claims_nothing():
    """`from_bag` on a dict claims nothing: there is no read in progress, and a
    hand-rolled bag may name no live surface at all."""
    frame = VideoFrame.from_bag(FRAME_BAG)
    assert getattr(frame, CLAIM_FIELD) is None


def test_a_surface_that_cannot_be_claimed_still_reads(offered):
    """A frame whose surface is already gone is still a delivered frame. It
    falls back to what an untyped read gets — pool depth — rather than turning
    into an exception at the read."""
    offered(GpuLimitedAccessThatRefuses())

    frame = VideoFrame(**FRAME_BAG)

    assert getattr(frame, CLAIM_FIELD) is None
    assert frame.surface_id == "surface-7"


def test_the_claim_is_not_part_of_what_a_frame_is(offered):
    """It rides alongside the fields: two frames off the same bag are equal,
    and the claim shows up in neither equality nor repr."""
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)
    same_bag_again = VideoFrame(**FRAME_BAG)

    assert frame == same_bag_again
    assert CLAIM_FIELD not in repr(frame)


# ---- one frame, whichever spelling built it --------------------------------


def test_the_read_spelling_casts_nested_metadata_like_from_bag():
    """`read(port, into=VideoFrame)` constructs `VideoFrame(**bag)`, so that
    spelling has to produce the same frame `from_bag` does — otherwise the
    strictness dial hands back raw dicts where it promised types."""
    bag = {
        **FRAME_BAG,
        "fps": 30,
        "color_info": {"primaries": "bt709", "transfer": "srgb"},
        "content_light": {"max_cll": 1000, "max_fall": 400},
    }

    constructed = VideoFrame(**bag)

    assert constructed == VideoFrame.from_bag(bag)
    assert constructed.color_info == ColorInfo(primaries="bt709", transfer="srgb")
    assert constructed.content_light == ContentLight(max_cll=1000, max_fall=400)


def test_the_read_spelling_validates_like_from_bag():
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame(**{**FRAME_BAG, "timestamp_ns": "not-an-int"})
    with pytest.raises(ValueError, match="fps"):
        VideoFrame(**{**FRAME_BAG, "fps": "30"})
    with pytest.raises(ValueError, match="content_light"):
        VideoFrame(**{**FRAME_BAG, "content_light": {"max_cll": 1, "unexpected_key": 1}})


def test_already_cast_metadata_survives_construction():
    """A frame built in Python — a test fixture, a processor forwarding one —
    passes real `ColorInfo`, and re-casting must not mangle it."""
    frame = VideoFrame(**FRAME_BAG, color_info=ColorInfo(matrix="bt709"))
    assert frame.color_info == ColorInfo(matrix="bt709")
