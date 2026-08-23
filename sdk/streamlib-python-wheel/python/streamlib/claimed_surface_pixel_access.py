# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The tensor protocol a cast type composes to reach its own pixels.

The object `ctx.inputs.read(port, into=T)` hands back is the tensor-protocol
producer: compose `ClaimedSurfacePixelAccess` and `torch.from_dlpack(frame)`
works straight off the read, with no resolve and no lock in the caller's hands.

Nothing here is privileged. The claim rides the same public offer any
constructing class may already take, and `VideoFrame` is built from this piece
like any other cast type — which is what keeps the shipped frame from holding a
position a user-authored type cannot reach.

A write through the bare view is out of contract. DLPack's read-only flag
exists only in the versioned exchange and consumers may ignore it, so the lock
this holds declares read intent rather than enforcing it — the same honest
posture as the raw-handle use bound.
"""

from __future__ import annotations

import dataclasses
from typing import Any

from ._engine import (
    GpuContextLimitedAccess,
    GpuSurfaceCheckOutLease,
    GpuSurfaceHandle,
    gpu_limited_access_of_the_typed_read_in_progress,
)
from .log import warn

__all__ = ["ClaimedSurfacePixelAccess"]

_THE_FIELD_A_CAST_TYPE_NAMES_ITS_SURFACE_WITH_BY_DEFAULT = "surface_id"

_a_refused_claim_has_been_reported = False


def _report_the_first_refused_claim(surface_id: str, refusal: BaseException) -> None:
    """Say once, per process, that frames are arriving unprotected.

    Once and not per frame: this is the per-frame path, and a helper's records
    cross to the parent, so a refusal that persists would cost more in logging
    than the claim it is reporting on. Silence would be worse than either — the
    whole lifetime contract can be off with no other signal.
    """
    global _a_refused_claim_has_been_reported
    if _a_refused_claim_has_been_reported:
        return
    _a_refused_claim_has_been_reported = True
    warn(
        "a frame could not claim its surface, so the producer may recycle it while this "
        "processor is still holding the frame; frames are protected by pool depth alone "
        "until this clears. Not reported again in this process.",
        surface_id=surface_id,
        refusal=str(refusal),
    )


def _claim_taken_on(
    gpu_limited_access: GpuContextLimitedAccess, surface_id: str
) -> "GpuSurfaceCheckOutLease | None":
    try:
        return gpu_limited_access.claim_surface_against_producer_reuse(surface_id)
    except Exception as refusal:  # noqa: BLE001 — see below
        # Deliberately every failure, not just the refusals this path is known
        # to raise today: the claim crosses a socket, and whatever comes back
        # from below it, none of it makes the delivered bag unreadable. An
        # unclaimed object falls back to the protection pool depth gives it,
        # which is what an untyped read gets; raising here would turn a
        # delivered frame into an exception at the read. Nothing is hidden by
        # the breadth — the first one is reported.
        _report_the_first_refused_claim(surface_id, refusal)
        return None


def _fields_the_cast_type_declared_built_from(
    cast_object: "ClaimedSurfacePixelAccess", bag_entries: "dict[str, Any]"
) -> None:
    """Assign the concrete type's declared dataclass fields from the bag.

    The bag is an open map, so a key this type does not declare is dropped
    rather than refused: the day a producer adds one must not be the day every
    typed read starts raising, which would take the claim with it.

    A composer that is not a dataclass declares nothing here and keeps its own
    constructor's state — this still takes its claim.
    """
    if not dataclasses.is_dataclass(cast_object):
        return
    assign = object.__setattr__
    for declared in dataclasses.fields(cast_object):
        if declared.name in bag_entries:
            assign(cast_object, declared.name, bag_entries[declared.name])
        elif declared.default is not dataclasses.MISSING:
            assign(cast_object, declared.name, declared.default)
        elif declared.default_factory is not dataclasses.MISSING:
            assign(cast_object, declared.name, declared.default_factory())
        else:
            raise ValueError(
                f"the bag does not build a {type(cast_object).__name__}: "
                f"missing key {declared.name!r}"
            )


class ClaimedSurfacePixelAccess:
    """Composed by a cast type to claim its surface and speak DLPack over it.

    The surface-naming field is declared, never guessed — it defaults to
    `surface_id` and a type that names its own passes it at class creation:
    `class DepthFrame(ClaimedSurfacePixelAccess, surface_id_field="depth_id")`.
    """

    #: Declared by the type that composed this, inherited by anything
    #: extending it.
    _the_field_this_cast_type_names_its_surface_with: str = (
        _THE_FIELD_A_CAST_TYPE_NAMES_ITS_SURFACE_WITH_BY_DEFAULT
    )

    # The three below carry a class-level `None` rather than a bare
    # annotation, so a composer that bypassed both construction hooks reaches
    # the refusal that names the read instead of an AttributeError.

    #: The claim on this object's own pixels, and its whole lifetime protocol:
    #: this object going away is what releases it. `None` when nothing offered
    #: the means to take one.
    _check_out_lease_on_the_claimed_surface: "GpuSurfaceCheckOutLease | None" = None

    #: Read once at construction and never again, so the view is over the
    #: surface the claim protects. Re-reading the declared field at reach time
    #: would let a composer that is not frozen point the two at different
    #: surfaces, which is the silent wrongness the lifetime contract exists to
    #: kill.
    _the_surface_id_the_claim_was_taken_on: "str | None" = None

    #: Kept so the pixels stay reachable after the read that offered it
    #: returns — the offer is withdrawn the moment construction ends, and the
    #: protocol methods run long after.
    _gpu_limited_access_that_offered_the_claim: "GpuContextLimitedAccess | None" = None

    #: The resolved surface behind the bare view, imported on first reach and
    #: released with this object. `None` until something asks for pixels.
    _read_only_locked_handle_on_the_claimed_surface: "GpuSurfaceHandle | None" = None

    def __init_subclass__(
        cls, surface_id_field: "str | None" = None, **class_creation_keywords: Any
    ) -> None:
        super().__init_subclass__(**class_creation_keywords)
        # Absent means "keep what the type being extended declared". A default
        # reapplied per class would silently re-point a subclass of a type that
        # named its own field back at `surface_id`.
        if surface_id_field is not None:
            cls._the_field_this_cast_type_names_its_surface_with = surface_id_field

    def __init__(self, **bag_entries: Any) -> None:
        """Build the declared fields from the bag's entries, then claim.

        This is what `@dataclass(frozen=True, init=False)` inherits. A type that
        writes its own constructor — to validate, or to cast nested metadata —
        calls this with the values it settled on, which is how `VideoFrame` is
        built.
        """
        _fields_the_cast_type_declared_built_from(self, bag_entries)
        self._take_the_claim_on(
            bag_entries.get(self._the_field_this_cast_type_names_its_surface_with)
        )

    def __post_init__(self) -> None:
        """The claim for a type whose `__init__` the dataclass decorator
        generated, which never routes through this class's own.

        A composer overriding this owes it a `super().__post_init__()`;
        without one the type silently claims nothing.
        """
        self._take_the_claim_on(
            getattr(self, self._the_field_this_cast_type_names_its_surface_with, None)
        )

    def _take_the_claim_on(self, surface_id: Any) -> None:
        gpu_limited_access = gpu_limited_access_of_the_typed_read_in_progress()
        claimed_surface_id = surface_id if isinstance(surface_id, str) else None
        assign = object.__setattr__
        # The capability is kept even when the claim below is refused: an
        # unclaimed object is one riding pool depth, not one with no pixels, so
        # reaching for its view must still reach the surface and be refused
        # *there*, by name.
        assign(self, "_gpu_limited_access_that_offered_the_claim", gpu_limited_access)
        assign(self, "_the_surface_id_the_claim_was_taken_on", claimed_surface_id)
        assign(self, "_read_only_locked_handle_on_the_claimed_surface", None)
        assign(
            self,
            "_check_out_lease_on_the_claimed_surface",
            _claim_taken_on(gpu_limited_access, claimed_surface_id)
            if gpu_limited_access is not None and claimed_surface_id is not None
            else None,
        )

    def _view_of_the_claimed_surface(self) -> GpuSurfaceHandle:
        """The resolved, read-only-locked surface the protocol methods export.

        Resolved on first reach rather than at construction: importing a
        surface's memory is real per-frame work, and most reads want the bag's
        metadata and never a pixel.
        """
        already_resolved = self._read_only_locked_handle_on_the_claimed_surface
        if already_resolved is not None:
            return already_resolved
        gpu_limited_access = self._gpu_limited_access_that_offered_the_claim
        if gpu_limited_access is None:
            raise RuntimeError(
                f"this {type(self).__name__} was not built by a typed read, so nothing "
                f"offered it the means to reach its surface's pixels. Read the bag with "
                f"`ctx.inputs.read(port, into={type(self).__name__})` — the view rides the "
                f"claim that read takes."
            )
        surface_id = self._the_surface_id_the_claim_was_taken_on
        if surface_id is None:
            raise RuntimeError(
                f"this {type(self).__name__} names no surface in "
                f"{self._the_field_this_cast_type_names_its_surface_with!r}, so there are "
                f"no pixels to export. A cast type declares the field its surface id "
                f"arrives in — `class {type(self).__name__}(ClaimedSurfacePixelAccess, "
                f'surface_id_field="…")` when it is not `surface_id`.'
            )
        handle = gpu_limited_access.resolve_surface(surface_id)
        # Read intent, declared: it is what keeps the export from arming a
        # write-back, and the write doors are the scopes, never this view.
        handle.lock(read_only=True)
        object.__setattr__(
            self, "_read_only_locked_handle_on_the_claimed_surface", handle
        )
        return handle

    def __dlpack_device__(self) -> "tuple[int, int]":
        """The DLPack device this object's pixels live on."""
        return self._view_of_the_claimed_surface().__dlpack_device__()

    def __dlpack__(
        self,
        stream: "Any | None" = None,
        max_version: "tuple[int, int] | None" = None,
        dl_device: "tuple[int, int] | None" = None,
        copy: "bool | None" = None,
    ) -> Any:
        """A DLPack capsule over this object's pixels, GPU-resident.

        Valid while this object lives — the claim is what holds the frame still
        — and ended when it drops. What a consumer negotiates reaches the
        surface unchanged: this is grammar over the handle, never a filter.
        """
        return self._view_of_the_claimed_surface().__dlpack__(
            stream=stream,
            max_version=max_version,
            dl_device=dl_device,
            copy=copy,
        )
