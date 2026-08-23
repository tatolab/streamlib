# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The composable that makes a cast object the tensor-protocol producer.

`[cast-object-tensor-protocol]` says the object `read(port, into=T)` hands back
speaks `__dlpack__` itself, and that the wheel ships the protocol as one public
piece any cast type composes — `VideoFrame` being built from it is the proof it
holds no privileged position. So the type under test here is a *user-authored*
one: if the protocol only worked for the class we ship, these would fail.

The capability is stood in for. The real one needs a running engine and a
surface-share service, which the wheel's Rust tests and the GPU-marked probes
in `test_cast_claim.py` cover; what is left for the GPU-free half is the
composable's own half — that it claims what its declared field names, absorbs
the resolve/lock ceremony behind the protocol methods, and lets go of both when
the object drops.
"""

from __future__ import annotations

import gc
import os
import weakref
from dataclasses import dataclass, field
from typing import Any

import pytest

from streamlib import (
    ClaimedSurfacePixelAccess,
    ProcessorLinkDataAccess,
    RuntimeContextFullAccess,
    VideoFrame,
)
from streamlib import claimed_surface_pixel_access as composable_module

FRAME_BAG = {
    "surface_id": "surface-7",
    "width_in_pixels": 1280,
    "height_in_pixels": 720,
}

PER_SURFACE_ACCESS_FIELD = "_pixel_access_by_declared_surface_field"
OUTPUT_PORT = "frames_to_downstream"
INPUT_PORT = "frames_from_upstream"


def claim_taken_on(
    cast_object: ClaimedSurfacePixelAccess, surface_id_field: str = "surface_id"
) -> Any:
    """The lease a cast object took for one of its declared surfaces.

    Read here rather than inferred from behaviour, so a test can say whether a
    claim was taken at all separately from whether reaching the pixels worked.
    """
    return cast_object.pixel_access_to_the_surface_declared_in(
        surface_id_field
    )._check_out_lease_on_the_claimed_surface


# ---- the types a user would write ------------------------------------------


@dataclass(frozen=True, init=False)
class DepthFrame(ClaimedSurfacePixelAccess):
    """The change file's own example: declare the fields, inherit the
    constructor, get the protocol."""

    surface_id: str
    width_in_pixels: int
    height_in_pixels: int
    units_per_metre: float = 1.0
    tags: list[str] = field(default_factory=list)


@dataclass(frozen=True, init=False)
class DepthFrameNamingItsOwnField(
    ClaimedSurfacePixelAccess, surface_id_field="depth_surface_id"
):
    depth_surface_id: str


@dataclass(frozen=True, init=False)
class DepthFrameWhoseSurfaceMayBeAbsent(
    ClaimedSurfacePixelAccess, surface_id_field="depth_surface_id"
):
    depth_surface_id: "str | None" = None


class ACastTypeThatIsNotADataclass(ClaimedSurfacePixelAccess):
    """Composing does not require being a dataclass: a type with its own
    constructor keeps its own state and still gets the claim."""

    def __init__(self, **bag_entries: Any) -> None:
        self.keys_that_arrived = sorted(bag_entries)
        super().__init__(**bag_entries)


class ACastTypeThatSettlesItsSurfaceItself(ClaimedSurfacePixelAccess):
    """Its own constructor decides which surface it names — here out of a
    nested map — before handing the rest to the composable."""

    def __init__(self, **bag_entries: Any) -> None:
        self.surface_id = bag_entries["surfaces"]["colour"]
        super().__init__(**bag_entries)


@dataclass(frozen=True, init=False)
class ColourAndDepthFrame(
    ClaimedSurfacePixelAccess,
    surface_id_fields=("colour_surface_id", "depth_surface_id"),
):
    """A cast type over two surfaces at once — an RGB-D camera's pair, the
    shape the bare protocol has no unambiguous answer for."""

    colour_surface_id: str
    depth_surface_id: str


@dataclass(frozen=True)
class DetectionOverlay(ClaimedSurfacePixelAccess):
    """The other spelling: an ordinary frozen dataclass whose `__init__` the
    decorator generates."""

    surface_id: str
    labels: list[str] = field(default_factory=list)


# ---- the capability the read offers ----------------------------------------


class ClaimStandingInForALease:
    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id


class HostArrayStandIn:
    """What `as_numpy()` hands back — the CPU door only forwards it."""

    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id


class DeviceTensorScopeStandIn:
    """What `as_device_tensor()` hands back: the scope whose own enter/exit is
    the whole write rule, which is why the GPU door adds nothing to it."""

    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id
        self.entered = False
        self.exit_arguments: "tuple[object, object, object] | None" = None

    def __enter__(self) -> "DeviceTensorScopeStandIn":
        self.entered = True
        return self

    def __exit__(self, exception_type, exception, traceback) -> bool:
        self.exit_arguments = (exception_type, exception, traceback)
        return False


class SurfaceHandleStandIn:
    """What `resolve_surface` hands back — the composable only locks it and
    forwards the protocol to it."""

    def __init__(self, surface_id: str) -> None:
        self.surface_id = surface_id
        self.locked_read_only: bool | None = None
        self.closed = False
        self.dlpack_calls: list[dict[str, object]] = []
        self.device_tensor_scopes: list[DeviceTensorScopeStandIn] = []
        self.lock_state_when_the_host_array_was_taken: "bool | None" = None
        self.exit_arguments: "tuple[object, object, object] | None" = None

    def lock(self, read_only: bool = True) -> None:
        self.locked_read_only = read_only

    def unlock(self) -> None:
        self.locked_read_only = None

    def close(self) -> None:
        self.closed = True

    def __enter__(self) -> "SurfaceHandleStandIn":
        return self

    def __exit__(self, exception_type, exception, traceback) -> bool:
        self.exit_arguments = (exception_type, exception, traceback)
        self.close()
        return False

    def as_device_tensor(self) -> DeviceTensorScopeStandIn:
        scope = DeviceTensorScopeStandIn(self.surface_id)
        self.device_tensor_scopes.append(scope)
        return scope

    def as_numpy(self) -> HostArrayStandIn:
        self.lock_state_when_the_host_array_was_taken = self.locked_read_only
        return HostArrayStandIn(self.surface_id)

    def __dlpack_device__(self) -> tuple[int, int]:
        return (2, 0)

    def __dlpack__(
        self,
        stream: object | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> str:
        self.dlpack_calls.append(
            {
                "stream": stream,
                "max_version": max_version,
                "dl_device": dl_device,
                "copy": copy,
            }
        )
        return f"capsule-over-{self.surface_id}"


class GpuLimitedAccessStandIn:
    """The shape a typed read offers, recording what it was asked for."""

    def __init__(self) -> None:
        self.claimed_surface_ids: list[str] = []
        self.resolved_surface_ids: list[str] = []
        self.handed_out_handles: list[SurfaceHandleStandIn] = []

    def claim_surface_against_producer_reuse(
        self, surface_id: str
    ) -> ClaimStandingInForALease:
        self.claimed_surface_ids.append(surface_id)
        return ClaimStandingInForALease(surface_id)

    def resolve_surface(self, surface_id: str) -> SurfaceHandleStandIn:
        self.resolved_surface_ids.append(surface_id)
        handle = SurfaceHandleStandIn(surface_id)
        self.handed_out_handles.append(handle)
        return handle


class GpuLimitedAccessThatRefuses:
    """A capability whose surface is already gone — the honest race."""

    def claim_surface_against_producer_reuse(self, surface_id: str) -> object:
        raise RuntimeError(
            f"the surface-share service refused check_out of {surface_id!r}: unknown surface"
        )

    def resolve_surface(self, surface_id: str) -> object:
        raise RuntimeError(
            f"the surface {surface_id!r} was recycled by its producer"
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


def test_a_user_authored_cast_type_claims_the_field_it_declares(offered):
    """The no-privilege claim, made concrete: a type the wheel never heard of
    gets exactly what `VideoFrame` gets."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = DepthFrame(**FRAME_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(frame).surface_id == "surface-7"


def test_a_type_that_declares_another_field_claims_that_one(offered):
    """Declared, never guessed — the composable reads the field the type named
    and never inspects the bag for something surface-shaped."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    DepthFrameNamingItsOwnField(depth_surface_id="depth-3", surface_id="colour-9")

    assert gpu_limited_access.claimed_surface_ids == ["depth-3"]


def test_a_type_extending_a_cast_type_keeps_the_declared_field(offered):
    """A sub-subclass must not silently fall back to the default: the field is
    the parent's declaration, not a per-class default reapplied."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    @dataclass(frozen=True, init=False)
    class DepthFrameWithConfidence(DepthFrameNamingItsOwnField):
        confidence: float = 0.0

    DepthFrameWithConfidence(depth_surface_id="depth-3", confidence=0.5)

    assert gpu_limited_access.claimed_surface_ids == ["depth-3"]


def test_a_generated_dataclass_constructor_claims_too(offered):
    """The other authoring spelling. A `@dataclass(frozen=True)` whose
    `__init__` the decorator generates must claim as well — a composer who
    wrote the obvious thing and silently got no claim is the failure mode the
    lifetime contract exists to kill."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    overlay = DetectionOverlay(surface_id="surface-7", labels=["cat"])

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert claim_taken_on(overlay).surface_id == "surface-7"


def test_constructed_outside_a_typed_read_it_claims_nothing():
    """Only a `read(port, into=…)` offers the means, so an object built from a
    bag you are holding claims nothing — a hand-rolled bag may name no live
    surface at all."""
    frame = DepthFrame(**FRAME_BAG)

    assert claim_taken_on(frame) is None


def test_the_claim_goes_away_with_the_object_and_nothing_is_called(offered):
    offered(GpuLimitedAccessStandIn())

    frame = DepthFrame(**FRAME_BAG)
    claim_still_alive = weakref.ref(claim_taken_on(frame))
    assert claim_still_alive() is not None

    del frame
    gc.collect()
    assert claim_still_alive() is None, (
        "the object going out of scope is the whole release protocol"
    )


def test_a_surface_that_cannot_be_claimed_still_constructs(offered, monkeypatch):
    """A frame whose surface is already gone is still a delivered frame; it
    falls back to what an untyped read gets rather than turning into an
    exception at the read."""
    offered(GpuLimitedAccessThatRefuses())
    monkeypatch.setattr(
        composable_module, "_a_refused_claim_has_been_reported", False
    )

    frame = DepthFrame(**FRAME_BAG)

    assert claim_taken_on(frame) is None
    assert frame.surface_id == "surface-7"


def test_a_refused_claim_is_reported_once_and_then_stays_quiet(offered, monkeypatch):
    """Silence would let the whole lifetime contract be off with no signal;
    per-frame logging would cost more than the claim it reports on."""
    offered(GpuLimitedAccessThatRefuses())
    monkeypatch.setattr(
        composable_module, "_a_refused_claim_has_been_reported", False
    )
    reported: list[str] = []
    monkeypatch.setattr(
        composable_module,
        "warn",
        lambda message, **attributes: reported.append(message),
    )

    DepthFrame(**FRAME_BAG)
    DepthFrame(**FRAME_BAG)

    assert len(reported) == 1, "the per-frame path must not flood the log"
    assert "pool depth" in reported[0]


def test_the_claim_is_not_part_of_what_the_object_is(offered):
    """It rides alongside the declared fields: two objects off the same bag are
    equal, and neither the claim nor the view shows up in equality or repr."""
    offered(GpuLimitedAccessStandIn())

    frame = DepthFrame(**FRAME_BAG)
    same_bag_again = DepthFrame(**FRAME_BAG)
    _ = frame.__dlpack__()

    assert frame == same_bag_again
    assert PER_SURFACE_ACCESS_FIELD not in repr(frame)
    assert "surface-7" in repr(frame), "the declared fields are still the repr"


def test_a_composer_that_is_not_a_dataclass_claims_from_the_bag(offered):
    """Nothing here declares a dataclass field, so the composable assigns
    nothing and the bag entry is the only place the surface id can come
    from — and the protocol still works over it."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    composed = ACastTypeThatIsNotADataclass(**FRAME_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert composed.keys_that_arrived == sorted(FRAME_BAG)
    assert composed.__dlpack__() == "capsule-over-surface-7"


def test_a_composer_that_settles_its_surface_first_claims_that_one(offered):
    """The claim and the view follow the attribute the type settled, never a
    same-named bag entry that disagrees with it.

    A type reading its surface id out of a nested map is the shape that makes
    the two diverge, and a claim on one surface guarding a view of another is
    the silent wrongness the lifetime contract exists to kill.
    """
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    composed = ACastTypeThatSettlesItsSurfaceItself(
        surfaces={"colour": "colour-9"}, surface_id="a-stale-id-the-bag-carried"
    )

    assert gpu_limited_access.claimed_surface_ids == ["colour-9"]
    assert composed.__dlpack__() == "capsule-over-colour-9"


# ---- construction from the bag's entries -----------------------------------


def test_keys_the_cast_type_does_not_declare_are_ignored():
    """The bag is an open map — the engine's own convention. A producer adding
    a key must not turn every typed read into a TypeError, because that read is
    the one that claims the frame."""
    frame = DepthFrame(**FRAME_BAG, a_key_a_future_producer_adds="ignored")

    assert frame == DepthFrame(**FRAME_BAG)
    assert not hasattr(frame, "a_key_a_future_producer_adds")


def test_a_declared_field_with_a_default_may_be_absent_from_the_bag():
    frame = DepthFrame(**FRAME_BAG)

    assert frame.units_per_metre == 1.0


def test_a_declared_field_built_by_a_factory_gets_its_own_value():
    """Two objects must not share one mutable default."""
    first = DepthFrame(**FRAME_BAG)
    second = DepthFrame(**FRAME_BAG)

    first.tags.append("cat")

    assert second.tags == []


def test_the_generated_constructor_spelling_refuses_an_undeclared_bag_key(offered):
    """Only the inherited constructor is open-map-safe.

    A `@dataclass(frozen=True)` enforces the signature the decorator generated,
    so a producer adding a key breaks *that* spelling at the read. Locked here
    because the composable serves both spellings and only one survives an open
    map — a reader choosing between them has to be able to see the difference.
    """
    offered(GpuLimitedAccessStandIn())

    assert DepthFrame(**FRAME_BAG, a_key_a_future_producer_adds="ignored")

    # Splatted rather than written as keywords, because that is what the read
    # does — `read(port, into=T)` calls `T(**bag)` with whatever the wire
    # carried, which no type checker gets to see.
    overlay_bag_from_a_newer_producer: "dict[str, Any]" = {
        "surface_id": "surface-7",
        "a_key_a_future_producer_adds": "ignored",
    }
    with pytest.raises(TypeError, match="a_key_a_future_producer_adds"):
        DetectionOverlay(**overlay_bag_from_a_newer_producer)


def test_a_missing_declared_field_is_refused_naming_the_key():
    with pytest.raises(ValueError, match="missing key 'height_in_pixels'"):
        DepthFrame(surface_id="surface-7", width_in_pixels=1280)


# ---- the bare tensor protocol ----------------------------------------------


def test_the_bare_object_hands_back_the_surfaces_own_capsule(offered):
    """`torch.from_dlpack(frame)` off a read, with no resolve and no lock in
    the caller's hands — the ceremony is absorbed, not removed."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    capsule = frame.__dlpack__()

    assert capsule == "capsule-over-surface-7"
    assert gpu_limited_access.resolved_surface_ids == ["surface-7"]


def test_the_view_is_taken_under_a_read_only_lock(offered):
    """A write through the bare view is out of contract, so the lock the
    composable holds declares read intent — which is also what keeps the
    handle from arming a write-back."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    frame.__dlpack__()

    assert gpu_limited_access.handed_out_handles[0].locked_read_only is True


def test_the_device_the_object_advertises_is_the_surfaces_own(offered):
    offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    assert frame.__dlpack_device__() == (2, 0)


def test_the_two_protocol_calls_resolve_the_surface_once(offered):
    """A consumer asks for the device and then for the capsule. Resolving twice
    would import the surface's memory twice per frame on the hot read path."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    frame.__dlpack_device__()
    frame.__dlpack__()
    frame.__dlpack__()

    assert gpu_limited_access.resolved_surface_ids == ["surface-7"]


def test_what_the_consumer_negotiated_reaches_the_surface_unchanged(offered):
    """The composable is grammar over the handle, never a filter: a consumer
    negotiating the versioned exchange or asking for the host side must reach
    the same answer it would through `resolve_surface`."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    frame.__dlpack__(stream=7, max_version=(1, 0), dl_device=(1, 0), copy=False)

    assert gpu_limited_access.handed_out_handles[0].dlpack_calls == [
        {"stream": 7, "max_version": (1, 0), "dl_device": (1, 0), "copy": False}
    ]


def test_the_view_is_released_when_the_cast_object_drops(offered):
    """Validity rides the claim: the object dropping ends the view, and the
    handle it was holding goes with it."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)
    frame.__dlpack__()
    handle_still_alive = weakref.ref(gpu_limited_access.handed_out_handles[0])
    gpu_limited_access.handed_out_handles.clear()

    del frame
    gc.collect()

    assert handle_still_alive() is None


def test_the_protocol_outside_a_typed_read_refuses_naming_the_read():
    """Nothing offered the means to reach the pixels, so there is no view to
    hand back — and the refusal says which spelling would have one."""
    frame = DepthFrame(**FRAME_BAG)

    with pytest.raises(RuntimeError, match="into="):
        frame.__dlpack__()
    with pytest.raises(RuntimeError, match="into="):
        frame.__dlpack_device__()


def test_an_object_naming_no_surface_is_refused_pointing_at_the_declaration(offered):
    """A bag that carried no surface id claimed nothing and has nothing to
    export — and the refusal names the field the type declared rather than
    guessing at some other key that might look surface-shaped."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrameWhoseSurfaceMayBeAbsent()

    assert gpu_limited_access.claimed_surface_ids == []
    with pytest.raises(RuntimeError, match="names no surface in 'depth_surface_id'"):
        frame.__dlpack__()


def test_a_refused_claim_leaves_the_protocol_reachable_and_loud(offered):
    """An unclaimed frame is not a frame with no pixels — it is a frame riding
    pool depth. Reaching for its view must reach the surface and be refused
    *there*, loudly, rather than silently answering as if nothing was on
    offer."""
    offered(GpuLimitedAccessThatRefuses())
    frame = DepthFrame(**FRAME_BAG)

    with pytest.raises(RuntimeError, match="recycled"):
        frame.__dlpack__()


# ---- the GPU write door ----------------------------------------------------


def test_the_gpu_write_door_hands_back_the_surfaces_own_device_tensor_scope(offered):
    """`writable()` is grammar, not machinery: the scope it returns is the one
    the surface mints, whose own enter/exit already *is* the write rule —
    publish at the block edge, discard on a propagating exception."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    with frame.writable() as device_tensor:
        pass

    handle = gpu_limited_access.handed_out_handles[0]
    assert handle.device_tensor_scopes == [device_tensor]
    assert handle.device_tensor_scopes[0].entered is True
    assert handle.device_tensor_scopes[0].exit_arguments == (None, None, None)


def test_a_raise_inside_the_gpu_write_door_reaches_the_scopes_own_exit(offered):
    """The discard is the scope's, so what this owes is that the exception
    reaches it — and that nothing here suppresses the raise."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    with pytest.raises(ValueError, match="the edit went wrong"):
        with frame.writable():
            raise ValueError("the edit went wrong")

    scope = gpu_limited_access.handed_out_handles[0].device_tensor_scopes[0]
    assert scope.exit_arguments is not None
    assert scope.exit_arguments[0] is ValueError


def test_the_gpu_write_door_does_not_ride_the_read_paths_lock(offered):
    """Entering the scope is the write declaration, structurally — so the door
    takes no CPU lock, and a read-only one taken on the way in would be a
    declaration of the opposite intent sitting under a write."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    frame.writable()

    assert gpu_limited_access.handed_out_handles[0].locked_read_only is None


def test_the_gpu_write_door_shares_the_surface_the_read_path_resolved(offered):
    """Both doors are onto one claimed surface. Resolving a second time per
    frame would import the same memory twice on the hot path."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    frame.__dlpack__()
    frame.writable()

    assert gpu_limited_access.resolved_surface_ids == ["surface-7"]


# ---- the CPU write door ----------------------------------------------------


def test_the_cpu_door_yields_the_host_array_under_a_write_lock(offered):
    """`read_only=False` is what marks the array writable, so the lock must be
    open for writing at the moment the view is taken — not merely at some
    point inside the block."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    with frame.cpu() as host_array:
        assert host_array.surface_id == "surface-7"

    handle = gpu_limited_access.handed_out_handles[-1]
    assert handle.lock_state_when_the_host_array_was_taken is False


def test_the_cpu_door_leaves_through_the_surfaces_own_scope(offered):
    """Publication at the block edge is the handle's own exit — the same code
    that publishes a pending write and then releases."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    with frame.cpu():
        pass

    handle = gpu_limited_access.handed_out_handles[-1]
    assert handle.exit_arguments == (None, None, None)
    assert handle.closed is True


def test_a_raise_inside_the_cpu_door_reaches_that_scopes_exit_and_propagates(
    offered,
):
    """The discard-on-raise is the handle's, keyed on the exception reaching
    its `__exit__` — and the raise is never suppressed on the way."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    with pytest.raises(ValueError, match="the edit went wrong"):
        with frame.cpu():
            raise ValueError("the edit went wrong")

    handle = gpu_limited_access.handed_out_handles[-1]
    assert handle.exit_arguments is not None
    assert handle.exit_arguments[0] is ValueError
    assert handle.closed is True


def test_the_cpu_door_takes_its_own_surface_and_leaves_the_read_view_alone(
    offered,
):
    """The slow path pays a resolve of its own so that opening it cannot flip
    the bare view's intent underneath it.

    Sharing one handle would leave the read view locked for *writing* for the
    length of the block, and a bare `__dlpack__` taken in there would hand back
    a writable device tensor and arm a write-back nobody asked for.
    """
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)
    frame.__dlpack__()
    read_view = gpu_limited_access.handed_out_handles[0]

    with frame.cpu():
        assert read_view.locked_read_only is True

    assert gpu_limited_access.resolved_surface_ids == ["surface-7", "surface-7"]
    assert read_view.closed is False


# ---- a type that claims more than one surface -------------------------------


TWO_SURFACE_BAG = {
    "colour_surface_id": "colour-9",
    "depth_surface_id": "depth-3",
}


def test_a_type_claiming_two_surfaces_claims_both(offered):
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = ColourAndDepthFrame(**TWO_SURFACE_BAG)

    assert gpu_limited_access.claimed_surface_ids == ["colour-9", "depth-3"]
    assert claim_taken_on(frame, "colour_surface_id").surface_id == "colour-9"
    assert claim_taken_on(frame, "depth_surface_id").surface_id == "depth-3"


@pytest.mark.parametrize(
    "reach_for_the_bare_protocol",
    [
        lambda frame: frame.__dlpack__(),
        lambda frame: frame.__dlpack_device__(),
        lambda frame: frame.writable(),
        lambda frame: frame.cpu().__enter__(),
    ],
    ids=["__dlpack__", "__dlpack_device__", "writable", "cpu"],
)
def test_a_two_surface_type_is_refused_every_bare_door_naming_the_surfaces(
    offered, reach_for_the_bare_protocol
):
    """Guessing a primary surface silently is the wrongness the lifetime
    contract exists to kill, so every door onto "the" surface refuses — and the
    refusal names both surfaces and the accessor that reaches each one."""
    offered(GpuLimitedAccessStandIn())
    frame = ColourAndDepthFrame(**TWO_SURFACE_BAG)

    with pytest.raises(RuntimeError) as refusal:
        reach_for_the_bare_protocol(frame)

    assert "'colour_surface_id'" in str(refusal.value)
    assert "'colour-9'" in str(refusal.value)
    assert "'depth_surface_id'" in str(refusal.value)
    assert "'depth-3'" in str(refusal.value)
    assert "pixel_access_to_the_surface_declared_in" in str(refusal.value)


def test_each_surface_of_a_two_surface_type_is_reached_through_its_own_object(
    offered,
):
    """What the bare spelling has no unambiguous answer for, the per-surface
    object answers exactly: one protocol object per claimed surface, each over
    its own."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = ColourAndDepthFrame(**TWO_SURFACE_BAG)

    colour = frame.pixel_access_to_the_surface_declared_in("colour_surface_id")
    depth = frame.pixel_access_to_the_surface_declared_in("depth_surface_id")

    assert colour.__dlpack__() == "capsule-over-colour-9"
    assert depth.__dlpack__() == "capsule-over-depth-3"
    assert gpu_limited_access.resolved_surface_ids == ["colour-9", "depth-3"]


def test_both_write_doors_are_reachable_per_surface(offered):
    """The gradient is not a privilege of single-surface types: each surface of
    a multi-surface cast gets all three doors onto it."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())
    frame = ColourAndDepthFrame(**TWO_SURFACE_BAG)
    depth = frame.pixel_access_to_the_surface_declared_in("depth_surface_id")

    with depth.writable():
        pass
    with depth.cpu() as host_array:
        assert host_array.surface_id == "depth-3"

    assert gpu_limited_access.resolved_surface_ids == ["depth-3", "depth-3"]
    resolved_for_the_gpu_door = gpu_limited_access.handed_out_handles[0]
    assert resolved_for_the_gpu_door.device_tensor_scopes[0].surface_id == "depth-3"


def test_the_accessor_refuses_a_field_the_type_never_declared(offered):
    """Declared, never guessed, in both directions: asking for a surface this
    type does not name is refused with the names it does."""
    offered(GpuLimitedAccessStandIn())
    frame = ColourAndDepthFrame(**TWO_SURFACE_BAG)

    with pytest.raises(RuntimeError) as refusal:
        frame.pixel_access_to_the_surface_declared_in("infrared_surface_id")

    assert "'infrared_surface_id'" in str(refusal.value)
    assert "'colour_surface_id'" in str(refusal.value)
    assert "'depth_surface_id'" in str(refusal.value)


def test_a_single_surface_type_reaches_its_one_surface_through_the_accessor_too(
    offered,
):
    """One spelling for every cast type: the bare doors are the unambiguous
    case's shorthand for this, never a separate mechanism."""
    offered(GpuLimitedAccessStandIn())
    frame = DepthFrame(**FRAME_BAG)

    through_the_accessor = frame.pixel_access_to_the_surface_declared_in("surface_id")

    assert through_the_accessor.__dlpack__() == "capsule-over-surface-7"
    assert frame.__dlpack__() == "capsule-over-surface-7"


def test_declaring_a_surface_field_both_ways_at_once_is_refused():
    """One declaration per type. Silently preferring one keyword would make the
    other read as honoured when it was dropped."""
    with pytest.raises(TypeError, match="surface_id_field"):

        @dataclass(frozen=True, init=False)
        class ATypeThatDeclaresItsSurfacesTwice(
            ClaimedSurfacePixelAccess,
            surface_id_field="colour_surface_id",
            surface_id_fields=("colour_surface_id", "depth_surface_id"),
        ):
            colour_surface_id: str


def test_declaring_the_same_surface_field_twice_is_refused():
    with pytest.raises(TypeError, match="depth_surface_id"):

        @dataclass(frozen=True, init=False)
        class ATypeThatRepeatsASurfaceField(
            ClaimedSurfacePixelAccess,
            surface_id_fields=("depth_surface_id", "depth_surface_id"),
        ):
            depth_surface_id: str


def test_declaring_no_surface_field_at_all_is_refused():
    """An empty declaration is a type with no pixels to reach, which is a
    cast type that had no reason to compose this at all."""
    with pytest.raises(TypeError, match="at least one"):

        @dataclass(frozen=True, init=False)
        class ATypeThatDeclaresNoSurface(
            ClaimedSurfacePixelAccess, surface_id_fields=()
        ):
            width_in_pixels: int


# ---- the frame the wheel ships is one of these ------------------------------


def test_the_shipped_video_frame_is_built_from_this_piece(offered):
    """The proof of no privilege: the frame the wheel ships takes its claim and
    exports its pixels through exactly the code a user-authored cast type
    composes. A `VideoFrame` that stopped being one of these would be a private
    path back."""
    gpu_limited_access = offered(GpuLimitedAccessStandIn())

    frame = VideoFrame(surface_id="surface-7", width=1280, height=720, timestamp_ns=1)

    assert isinstance(frame, ClaimedSurfacePixelAccess)
    assert gpu_limited_access.claimed_surface_ids == ["surface-7"]
    assert frame.__dlpack__() == "capsule-over-surface-7"
    assert gpu_limited_access.handed_out_handles[0].locked_read_only is True


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
