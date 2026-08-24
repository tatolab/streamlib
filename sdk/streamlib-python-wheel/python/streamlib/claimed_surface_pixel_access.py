# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The tensor protocol a cast type composes to reach its own pixels.

The object `ctx.inputs.read(port, into=T)` hands back is the tensor-protocol
producer: compose `ClaimedSurfacePixelAccess` and `torch.from_dlpack(frame)`
works straight off the read, with no resolve and no lock in the caller's hands.
The gradient reads bare first, then the two write doors — `writable()` for a
GPU edit, `cpu()` for the CPU reach, whose name is the whole warning.

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
from collections.abc import Iterator, Sequence
from contextlib import AbstractContextManager, contextmanager
from typing import Any

from ._engine import (
    GpuContextLimitedAccess,
    GpuSurfaceCheckOutLease,
    GpuSurfaceDeviceTensorScope,
    GpuSurfaceHandle,
    gpu_limited_access_of_the_typed_read_in_progress,
)
from .log import warn

__all__ = ["ClaimedSurfacePixelAccess", "PixelAccessToOneClaimedSurface"]

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


_a_read_only_cpu_door_has_been_reported = False


def _report_the_first_read_only_cpu_door(surface_id: str) -> None:
    """Say once, per process, that `cpu()` is handing out read-only arrays.

    Once and not per frame, for the same reason as the refused-claim report:
    this is the per-frame path. numpy's own ValueError at the write is the
    per-use signal; what it does not name is the rule, so this does, before
    a bare "assignment destination is read-only" is anyone's first contact
    with it.
    """
    global _a_read_only_cpu_door_has_been_reported
    if _a_read_only_cpu_door_has_been_reported:
        return
    _a_read_only_cpu_door_has_been_reported = True
    warn(
        "cpu() is handing out read-only arrays for this surface: its frame cannot take a "
        "write-back — it is a pool member its producer still owns, or a foreign registration "
        "whose texture cannot take a copy in — so no in-place edit publishes through any door; "
        "an edit "
        "would land where other holders never see it. writable() refuses the same frames "
        "by name. Not reported again in this process.",
        surface_id=surface_id,
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


def _no_typed_read_offered_the_means_error(cast_type_name: str) -> RuntimeError:
    return RuntimeError(
        f"this {cast_type_name} was not built by a typed read, so nothing offered it the "
        f"means to reach its surface's pixels. Read the bag with "
        f"`ctx.inputs.read(port, into={cast_type_name})` — the view rides the claim that "
        f"read takes."
    )


def _build_the_fields_the_cast_type_declared(
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


class PixelAccessToOneClaimedSurface:
    """One claimed surface's pixels, and the three doors onto them.

    A cast type that names a single surface reaches these through the cast
    object itself; one that names several reaches each surface's own through
    `pixel_access_to_the_surface_declared_in`, because a bare view over a type
    holding two surfaces would have to guess which.

    Built by the composable and never by hand: the claim it carries was taken
    during a typed read, on the id read out of the field the type declared.
    """

    def __init__(
        self,
        gpu_limited_access_that_offered_the_claim: "GpuContextLimitedAccess | None",
        surface_id: "str | None",
        the_field_the_surface_id_was_declared_in: str,
        the_cast_type_that_declared_this_surface: str,
    ) -> None:
        # Kept even when the claim below is refused: an unclaimed surface is
        # one riding pool depth, not one with no pixels, so reaching for its
        # view must still reach the surface and be refused *there*, by name.
        self._gpu_limited_access_that_offered_the_claim = (
            gpu_limited_access_that_offered_the_claim
        )
        # Read once at construction and never again, so the view is over the
        # surface the claim protects. Re-reading the declared field at reach
        # time would let a composer that is not frozen point the two at
        # different surfaces, which is the silent wrongness the lifetime
        # contract exists to kill.
        self._the_surface_id_the_claim_was_taken_on = surface_id
        self._the_field_the_surface_id_was_declared_in = (
            the_field_the_surface_id_was_declared_in
        )
        self._the_cast_type_that_declared_this_surface = (
            the_cast_type_that_declared_this_surface
        )
        # This surface's claim, and its whole lifetime protocol: this object
        # going away is what releases it.
        self._check_out_lease_on_the_claimed_surface: (
            "GpuSurfaceCheckOutLease | None"
        ) = (
            _claim_taken_on(gpu_limited_access_that_offered_the_claim, surface_id)
            if gpu_limited_access_that_offered_the_claim is not None
            and surface_id is not None
            else None
        )
        # Resolved on first reach and released with this object.
        self._resolved_handle_on_the_claimed_surface: "GpuSurfaceHandle | None" = None
        self._the_read_only_lock_has_been_taken = False

    def _the_capability_and_the_claimed_surface_id(
        self,
    ) -> "tuple[GpuContextLimitedAccess, str]":
        cast_type = self._the_cast_type_that_declared_this_surface
        gpu_limited_access = self._gpu_limited_access_that_offered_the_claim
        if gpu_limited_access is None:
            raise _no_typed_read_offered_the_means_error(cast_type)
        surface_id = self._the_surface_id_the_claim_was_taken_on
        if surface_id is None:
            raise RuntimeError(
                f"this {cast_type} names no surface in "
                f"{self._the_field_the_surface_id_was_declared_in!r}, so there are no "
                f"pixels to export. A cast type declares the field its surface id "
                f'arrives in — `class {cast_type}(ClaimedSurfacePixelAccess, '
                f'surface_id_field="…")` when it is not `surface_id`.'
            )
        return gpu_limited_access, surface_id

    @property
    def surface_id_the_claim_was_taken_on(self) -> "str | None":
        """The id this object's claim was taken on, or `None` when the field
        the cast type declared held no surface id.

        The id read once at construction, so what a caller names downstream —
        a window's `show()`, above all — is the surface the claim protects and
        never one a mutable composer re-pointed the field at since.

        The name says which id this is, not that a claim succeeded. It is
        present whether or not one was taken: a refused claim, and an object
        no typed read constructed (`VideoFrame.from_bag`, or one built by
        hand), both leave the frame riding pool depth, which still has pixels
        to name. Naming a surface the producer has since recycled is refused
        loudly wherever the id is used, never answered with another frame.
        """
        return self._the_surface_id_the_claim_was_taken_on

    def _resolved_surface(self) -> GpuSurfaceHandle:
        """The imported surface behind every door, resolved on first reach.

        Not at construction: importing a surface's memory is real per-frame
        work, and most reads want the bag's metadata and never a pixel.
        """
        already_resolved = self._resolved_handle_on_the_claimed_surface
        if already_resolved is not None:
            return already_resolved
        gpu_limited_access, surface_id = (
            self._the_capability_and_the_claimed_surface_id()
        )
        handle = gpu_limited_access.resolve_surface(surface_id)
        self._resolved_handle_on_the_claimed_surface = handle
        return handle

    def _read_only_locked_view(self) -> GpuSurfaceHandle:
        """The resolved surface under a read-only lock — the bare path's view.

        Read intent, declared: it is what keeps the export from arming a
        write-back. A write through the bare view stays out of contract —
        never enforced, and never claimed to be.
        """
        handle = self._resolved_surface()
        if not self._the_read_only_lock_has_been_taken:
            handle.lock(read_only=True)
            self._the_read_only_lock_has_been_taken = True
        return handle

    def __dlpack_device__(self) -> "tuple[int, int]":
        """The DLPack device this surface's pixels live on."""
        return self._read_only_locked_view().__dlpack_device__()

    def __dlpack__(
        self,
        stream: "Any | None" = None,
        max_version: "tuple[int, int] | None" = None,
        dl_device: "tuple[int, int] | None" = None,
        copy: "bool | None" = None,
    ) -> Any:
        """A DLPack capsule over this surface's pixels, GPU-resident.

        Valid while the cast object lives — the claim is what holds the frame
        still — and ended when it drops. What a consumer negotiates reaches the
        surface unchanged: this is grammar over the handle, never a filter.
        """
        return self._read_only_locked_view().__dlpack__(
            stream=stream,
            max_version=max_version,
            dl_device=dl_device,
            copy=copy,
        )

    def writable(self) -> GpuSurfaceDeviceTensorScope:
        """The GPU write door: a scope over a device tensor of these pixels.

        `with frame.writable() as t:` — entering blits the surface out to a
        linear device view, leaving normally blits the edit back ordered ahead
        of the engine's next read, and leaving by a propagating exception
        discards it without suppressing the raise. `torch.from_dlpack(t)`
        inside the block is what a third-party GPU package edits in place.

        It takes no CPU lock: entering the scope *is* the write declaration,
        and a read-only lock underneath a write would declare the opposite.
        """
        return self._resolved_surface().as_device_tensor()

    @contextmanager
    def cpu(self) -> Iterator[Any]:
        """The CPU write door: a writable numpy array over these pixels.

        `with frame.cpu() as img:` — the slow path, named so. The array is
        writable exactly when the engine says the frame can take a write-back;
        a frame that cannot arrives read-only, enforced by numpy, under the
        same rule that makes `writable()` refuse it — no door writes where
        other holders never see.

        One contract across both backings: a raise leaves the frame the engine
        already held or a complete edit of fewer pixels, never a torn frame —
        which of the two is the backing's own, so code that must not publish on
        failure edits outside the scope.

        Over a pixel buffer the array *is* the surface's own coherent host
        mapping, so publication is per store: a raise mid-edit leaves the
        stores that already landed. Over a texture — a kernel's output, a
        texture this processor acquired — it is the surface's host-visible
        staging: entering reads the frame in, the writable array publishes at
        the block edge, and a propagating raise discards the edit instead. No
        door names the backing.

        What the block edge settles either way: the write intent ends, the
        scope closes, and a propagating exception is never suppressed.

        It resolves a surface of its own rather than sharing the bare view's:
        a shared handle would spend the block locked for *writing*, so a bare
        `__dlpack__` taken inside would hand back a writable device tensor and
        arm a write-back nobody asked for.
        """
        gpu_limited_access, surface_id = (
            self._the_capability_and_the_claimed_surface_id()
        )
        with gpu_limited_access.resolve_surface(surface_id) as surface:
            frame_takes_the_edit = gpu_limited_access.surface_can_take_write_back(
                surface_id
            )
            surface.lock(read_only=not frame_takes_the_edit)
            if not frame_takes_the_edit:
                _report_the_first_read_only_cpu_door(surface_id)
            yield surface.as_numpy()


class ClaimedSurfacePixelAccess:
    """Composed by a cast type to claim its surfaces and speak DLPack over them.

    The surface-naming field is declared, never guessed — it defaults to
    `surface_id` and a type that names its own passes it at class creation:
    `class DepthFrame(ClaimedSurfacePixelAccess, surface_id_field="depth_id")`.
    A type over more than one surface declares them together instead, with
    `surface_id_fields=("colour_id", "depth_id")`, and reaches each through
    `pixel_access_to_the_surface_declared_in` — the bare doors below refuse a
    type holding several rather than guess which surface was meant.

    `@dataclass(frozen=True, init=False)` is the spelling to write. Inheriting
    this class's constructor is what makes a cast type survive an open map: the
    wire's bag carries whatever its producer puts there, and a type that
    refuses an undeclared key turns the day a producer adds one into the day
    every typed read raises. A `@dataclass(frozen=True)` whose `__init__` the
    decorator generates does claim — through `__post_init__` — but enforces its
    own signature, so it is only safe against a bag whose keys it fully
    controls.
    """

    #: Declared by the type that composed this, inherited by anything
    #: extending it.
    _the_fields_this_cast_type_names_its_surfaces_with: "tuple[str, ...]" = (
        _THE_FIELD_A_CAST_TYPE_NAMES_ITS_SURFACE_WITH_BY_DEFAULT,
    )

    #: One protocol object per declared surface, each holding that surface's
    #: claim. A class-level `None` rather than a bare annotation, so a composer
    #: that bypassed both construction hooks reaches the refusal that names the
    #: read instead of an AttributeError.
    _pixel_access_by_declared_surface_field: (
        "dict[str, PixelAccessToOneClaimedSurface] | None"
    ) = None

    def __init_subclass__(
        cls,
        surface_id_field: "str | None" = None,
        surface_id_fields: "Sequence[str] | None" = None,
        **class_creation_keywords: Any,
    ) -> None:
        super().__init_subclass__(**class_creation_keywords)
        if surface_id_field is not None and surface_id_fields is not None:
            raise TypeError(
                f"{cls.__name__} declares its surfaces both ways at once: pass "
                f"surface_id_field for a type over one surface or surface_id_fields for "
                f"a type over several, never both"
            )
        if surface_id_field is not None:
            cls._the_fields_this_cast_type_names_its_surfaces_with = (surface_id_field,)
        elif surface_id_fields is not None:
            # A `str` is itself a `Sequence[str]`, so the plural keyword would
            # otherwise take one field name apart into a surface per character
            # — past the empty and duplicate checks, into a type that claims
            # nothing and refuses every door naming single letters. The
            # singular keyword next door takes a bare string, which is what
            # makes the slip worth naming.
            if isinstance(surface_id_fields, str):
                raise TypeError(
                    f"{cls.__name__} passed one field name, {surface_id_fields!r}, to "
                    f"surface_id_fields, which reads it as one surface per character. "
                    f"For a type over one surface pass "
                    f"surface_id_field={surface_id_fields!r}; for several, a tuple of "
                    f"field names"
                )
            declared = tuple(surface_id_fields)
            if not declared:
                raise TypeError(
                    f"{cls.__name__} declares no surface at all: surface_id_fields names "
                    f"at least one field a surface id arrives in, and a cast type with no "
                    f"pixels to reach has no reason to compose ClaimedSurfacePixelAccess"
                )
            if len(set(declared)) != len(declared):
                raise TypeError(
                    f"{cls.__name__} declares the same surface field twice in "
                    f"{declared!r}: one claim per surface, so each field is named once"
                )
            cls._the_fields_this_cast_type_names_its_surfaces_with = declared
        # Absent means "keep what the type being extended declared". A default
        # reapplied per class would silently re-point a subclass of a type that
        # named its own fields back at `surface_id`.

    def __init__(self, **bag_entries: Any) -> None:
        """Build the declared fields from the bag's entries, then claim.

        This is what `@dataclass(frozen=True, init=False)` inherits. A type that
        writes its own constructor — to validate, or to cast nested metadata —
        calls this with the values it settled on, which is how `VideoFrame` is
        built.
        """
        _build_the_fields_the_cast_type_declared(self, bag_entries)
        # The settled attributes, so both construction hooks claim on the same
        # values; the bag entry is the fallback for a composer that declared no
        # dataclass fields for this to have assigned.
        declared_fields = self._the_fields_this_cast_type_names_its_surfaces_with
        self._take_the_claims_on(
            {
                field: getattr(self, field, bag_entries.get(field))
                for field in declared_fields
            }
        )

    def __post_init__(self) -> None:
        """The claims for a type whose `__init__` the dataclass decorator
        generated, which never routes through this class's own.

        That constructor enforces its own signature, so this spelling refuses
        bag keys the type does not declare — see the class doc. A composer
        overriding this owes it a `super().__post_init__()`; without one the
        type silently claims nothing.
        """
        declared_fields = self._the_fields_this_cast_type_names_its_surfaces_with
        self._take_the_claims_on(
            {field: getattr(self, field, None) for field in declared_fields}
        )

    def _take_the_claims_on(
        self, surface_id_by_declared_field: "dict[str, Any]"
    ) -> None:
        gpu_limited_access = gpu_limited_access_of_the_typed_read_in_progress()
        cast_type_name = type(self).__name__
        object.__setattr__(
            self,
            "_pixel_access_by_declared_surface_field",
            {
                declared_field: PixelAccessToOneClaimedSurface(
                    gpu_limited_access,
                    surface_id if isinstance(surface_id, str) else None,
                    declared_field,
                    cast_type_name,
                )
                for declared_field, surface_id in surface_id_by_declared_field.items()
            },
        )

    def pixel_access_to_the_surface_declared_in(
        self, surface_id_field: str
    ) -> PixelAccessToOneClaimedSurface:
        """The protocol object for one of this type's declared surfaces.

        The only spelling a multi-surface cast type has, and the one a
        single-surface type's bare doors are shorthand for.
        """
        by_declared_field = self._pixel_access_by_declared_surface_field
        if by_declared_field is None:
            raise _no_typed_read_offered_the_means_error(type(self).__name__)
        if surface_id_field not in by_declared_field:
            raise RuntimeError(
                f"a {type(self).__name__} declares no surface in {surface_id_field!r}; it "
                f"names its surfaces in "
                f"{', '.join(repr(field) for field in by_declared_field)}"
            )
        return by_declared_field[surface_id_field]

    def _the_one_claimed_surfaces_pixel_access(self) -> PixelAccessToOneClaimedSurface:
        by_declared_field = self._pixel_access_by_declared_surface_field
        if by_declared_field is None:
            raise _no_typed_read_offered_the_means_error(type(self).__name__)
        if len(by_declared_field) != 1:
            claimed = ", ".join(
                f"{declared_field!r} "
                f"({pixel_access._the_surface_id_the_claim_was_taken_on!r})"
                for declared_field, pixel_access in by_declared_field.items()
            )
            raise RuntimeError(
                f"a {type(self).__name__} claims {len(by_declared_field)} surfaces — "
                f"{claimed} — so a bare view over it would have to guess which one you "
                f"meant. Reach each surface through its own protocol object: "
                f"`frame.pixel_access_to_the_surface_declared_in("
                f"{next(iter(by_declared_field))!r})`."
            )
        return next(iter(by_declared_field.values()))

    def __dlpack_device__(self) -> "tuple[int, int]":
        """The DLPack device this object's pixels live on."""
        return self._the_one_claimed_surfaces_pixel_access().__dlpack_device__()

    def __dlpack__(
        self,
        stream: "Any | None" = None,
        max_version: "tuple[int, int] | None" = None,
        dl_device: "tuple[int, int] | None" = None,
        copy: "bool | None" = None,
    ) -> Any:
        """A DLPack capsule over this object's pixels, GPU-resident."""
        return self._the_one_claimed_surfaces_pixel_access().__dlpack__(
            stream=stream,
            max_version=max_version,
            dl_device=dl_device,
            copy=copy,
        )

    @property
    def surface_id_the_claim_was_taken_on(self) -> "str | None":
        """The id this object's claim was taken on — what names it downstream.

        Present whether or not a claim was granted, on the same terms the
        per-surface accessor states. A type over several surfaces takes the
        refusal the bare doors give rather than guessing which one was meant;
        reach each surface's own through
        `pixel_access_to_the_surface_declared_in`.
        """
        return (
            self._the_one_claimed_surfaces_pixel_access().surface_id_the_claim_was_taken_on
        )

    def writable(self) -> GpuSurfaceDeviceTensorScope:
        """The GPU write door onto this object's pixels."""
        return self._the_one_claimed_surfaces_pixel_access().writable()

    def cpu(self) -> "AbstractContextManager[Any]":
        """The CPU write door onto this object's pixels — the slow path."""
        return self._the_one_claimed_surfaces_pixel_access().cpu()
