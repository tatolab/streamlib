# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a `VideoFrame` does with the capability a typed read offers it.

The seam itself — a message crossing a real link, a real surface-share
service, the lease the pool reads — is proven against the runtime. What is
proven here is the frame's own half: it claims when something offers the
means, holds the claim in a field nobody has to touch, releases it by going
away, and stays an ordinary construction when nothing is on offer.

The capability is stood in for by an object with
`claim_surface_against_producer_reuse`, which is all the read offers.
"""

from __future__ import annotations

import gc
import weakref
from typing import Any

import pytest

from tatolab.stream import (
    ClaimedSurfacePixelAccess,
    ColorInfo,
    ContentLight,
    VideoFrame,
)
from tatolab.stream import claimed_surface_pixel_access as composable_module
from tatolab.stream.msgpack import pack


FRAME_ENTRIES: "dict[str, Any]" = {
    "surface_id": "surface-7",
    "width": 1280,
    "height": 720,
    "timestamp_ns": 123_456_789,
}

FRAME_PAYLOAD = pack(FRAME_ENTRIES)


PER_SURFACE_ACCESS_FIELD = "_pixel_access_by_declared_surface_field"


def claim_taken_on(frame: ClaimedSurfacePixelAccess) -> Any:
    """The lease a frame took on the surface it names."""
    return frame.pixel_access_to_the_surface_declared_in(
        "surface_id"
    )._check_out_lease_on_the_claimed_surface


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
            composable_module,
            "gpu_limited_access_of_the_typed_read_in_progress",
            lambda: gpu_limited_access,
        )
        return gpu_limited_access

    return offer


def test_a_frame_built_under_an_offer_claims_the_surface_it_names(offered):
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_ENTRIES)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(frame).surface_id == "surface-7"


def test_from_bytes_claims_when_a_typed_read_is_the_one_constructing(offered):
    """The claim follows the read, not the spelling: a frame a read target
    builds inside its own `from_bytes` is offered the claim too."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = VideoFrame.from_bytes(FRAME_PAYLOAD)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(frame).surface_id == "surface-7"


def test_from_bytes_outside_any_read_claims_nothing():
    """With no offer standing, a payload in hand builds an unclaimed frame."""
    frame = VideoFrame.from_bytes(FRAME_PAYLOAD)

    assert claim_taken_on(frame) is None


def test_the_claim_goes_away_with_the_frame_and_nothing_is_called(offered):
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_ENTRIES)
    claim_still_alive = weakref.ref(claim_taken_on(frame))
    assert claim_still_alive() is not None

    del frame
    gc.collect()
    assert claim_still_alive() is None, (
        "the frame going out of scope is the whole release protocol"
    )


def test_a_surface_that_cannot_be_claimed_still_reads(offered):
    """A frame whose surface is already gone is still a delivered frame; it
    falls back to pool depth rather than raising at the read."""
    offered(GpuLimitedAccessThatRefuses())

    frame = VideoFrame.from_bytes(FRAME_PAYLOAD)

    assert claim_taken_on(frame) is None
    assert frame.surface_id == "surface-7"


def test_the_claim_is_not_part_of_what_a_frame_is(offered):
    """Two frames off the same message are equal, and the claim shows up in
    neither equality nor repr."""
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame.from_bytes(FRAME_PAYLOAD)
    same_message_again = VideoFrame.from_bytes(FRAME_PAYLOAD)

    assert frame == same_message_again
    assert PER_SURFACE_ACCESS_FIELD not in repr(frame)


def test_to_bytes_carries_no_claim(offered):
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame.from_bytes(FRAME_PAYLOAD)

    assert PER_SURFACE_ACCESS_FIELD.encode() not in frame.to_bytes()
    assert VideoFrame.from_bytes(frame.to_bytes()) == frame


def test_from_bytes_casts_nested_metadata_like_keyword_construction():
    entries = {
        **FRAME_ENTRIES,
        "fps": 30,
        "color_info": {"primaries": "bt709", "transfer": "srgb"},
        "content_light": {"max_cll": 1000, "max_fall": 400},
    }

    read_back = VideoFrame.from_bytes(pack(entries))

    assert read_back == VideoFrame(**entries)
    assert read_back.color_info == ColorInfo(primaries="bt709", transfer="srgb")
    assert read_back.content_light == ContentLight(max_cll=1000, max_fall=400)


def test_keyword_construction_validates_like_from_bytes():
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame(**{**FRAME_ENTRIES, "timestamp_ns": "not-an-int"})
    with pytest.raises(ValueError, match="fps"):
        VideoFrame(**{**FRAME_ENTRIES, "fps": "30"})
    with pytest.raises(ValueError, match="content_light"):
        VideoFrame(**{**FRAME_ENTRIES, "content_light": {"max_cll": 1, "unexpected_key": 1}})


def test_both_spellings_ignore_keys_the_cast_does_not_read():
    """A message is an open map; a producer adding a key must not turn the
    read that claims the frame into a TypeError."""
    entries_from_a_newer_producer = {
        **FRAME_ENTRIES,
        "a_key_a_future_producer_adds": "ignored",
    }

    assert VideoFrame(**entries_from_a_newer_producer) == VideoFrame.from_bytes(
        pack(entries_from_a_newer_producer)
    )


def test_already_cast_metadata_survives_construction():
    """A frame built in Python passes real `ColorInfo`, and re-casting must
    not mangle it."""
    frame = VideoFrame(**FRAME_ENTRIES, color_info=ColorInfo(matrix="bt709"))
    assert frame.color_info == ColorInfo(matrix="bt709")
