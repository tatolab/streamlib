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
import os
import weakref

import pytest

from streamlib import (
    ClaimedSurfacePixelAccess,
    ColorInfo,
    ContentLight,
    ProcessorLinkDataAccess,
    RuntimeContextFullAccess,
    VideoFrame,
    gpu_limited_access_of_the_typed_read_in_progress,
)
from streamlib import claimed_surface_pixel_access as composable_module

FRAME_BAG = {
    "surface_id": "surface-7",
    "width": 1280,
    "height": 720,
    "timestamp_ns": 123_456_789,
}

PER_SURFACE_ACCESS_FIELD = "_pixel_access_by_declared_surface_field"
OUTPUT_PORT = "frames_to_downstream"
INPUT_PORT = "frames_from_upstream"


def claim_taken_on(frame: object) -> object:
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


# ---- the claim -------------------------------------------------------------


def test_a_frame_built_under_an_offer_claims_the_surface_it_names(offered):
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(frame).surface_id == "surface-7"


def test_from_bag_claims_when_a_typed_read_is_the_one_constructing(offered):
    """The claim follows the read, not the spelling.

    A type reached through `read(port, into=MyType)` may build a `VideoFrame`
    from a nested bag inside its own constructor, and that frame is as much a
    delivered frame as one the read built directly — the offer is open for the
    whole construction, to every class it reaches. Documenting `from_bag` as
    unconditionally claimless would be wrong here, and would tell an author
    composing types that their nested frames are unprotected when they are not.
    """
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    class FrameHolderBuiltByTheRead:
        def __init__(self, **bag: object) -> None:
            self.frame = VideoFrame.from_bag(bag)

    holder = FrameHolderBuiltByTheRead(**FRAME_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(holder.frame).surface_id == "surface-7"


def test_from_bag_outside_any_read_claims_nothing():
    """The other half, with no offer standing: the same call takes no claim,
    which is what an author building a bag by hand gets."""
    frame = VideoFrame.from_bag(FRAME_BAG)

    assert claim_taken_on(frame) is None


def test_the_claim_goes_away_with_the_frame_and_nothing_is_called(offered):
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)
    claim_still_alive = weakref.ref(claim_taken_on(frame))
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
    assert claim_taken_on(frame) is None


def test_a_surface_that_cannot_be_claimed_still_reads(offered):
    """A frame whose surface is already gone is still a delivered frame. It
    falls back to what an untyped read gets — pool depth — rather than turning
    into an exception at the read."""
    offered(GpuLimitedAccessThatRefuses())

    frame = VideoFrame(**FRAME_BAG)

    assert claim_taken_on(frame) is None
    assert frame.surface_id == "surface-7"


def test_the_claim_is_not_part_of_what_a_frame_is(offered):
    """It rides alongside the fields: two frames off the same bag are equal,
    and the claim shows up in neither equality nor repr."""
    offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(**FRAME_BAG)
    same_bag_again = VideoFrame(**FRAME_BAG)

    assert frame == same_bag_again
    assert PER_SURFACE_ACCESS_FIELD not in repr(frame)


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


def test_both_spellings_ignore_keys_the_cast_does_not_read():
    """The bag is an open map — the engine's own convention, and what the Rust
    cast does. A producer adding a key must not turn every typed read into a
    TypeError, because that read is the one that claims the frame."""
    bag_from_a_newer_producer = {**FRAME_BAG, "a_key_a_future_producer_adds": "ignored"}

    assert VideoFrame(**bag_from_a_newer_producer) == VideoFrame.from_bag(
        bag_from_a_newer_producer
    )
    assert VideoFrame(**bag_from_a_newer_producer).surface_id == "surface-7"


def test_already_cast_metadata_survives_construction():
    """A frame built in Python — a test fixture, a processor forwarding one —
    passes real `ColorInfo`, and re-casting must not mangle it."""
    frame = VideoFrame(**FRAME_BAG, color_info=ColorInfo(matrix="bt709"))
    assert frame.color_info == ColorInfo(matrix="bt709")


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
    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, notify_service_name,
        "read_next_in_order", 8, 2, 1, True, link_id,
    )  # fmt: skip
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT, channel_service_name, notify_service_name,
        1024, 1 << 20, 8, 2, 1, True, link_id,
    )  # fmt: skip

    ctx = RuntimeContextFullAccess.open_for_helper_process(
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
