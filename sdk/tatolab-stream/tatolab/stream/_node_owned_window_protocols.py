# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The window a node owns, and the state one drain takes off it.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from ._gpu_protocols import GpuSurfaceHandle
    from .claimed_surface_pixel_access import ClaimedSurfacePixelAccess


class NodeOwnedWindow(Protocol):
    """A window this node owns, presented by the engine at vsync.

    Constructed in `setup()` through `ctx.gpu_full_access.create_window(...)`;
    named frames per frame in `process()`. No window handle, swapchain or
    present thread reaches Python — the object is the handle.
    """

    @property
    def title(self) -> str:
        """The title this window was requested with."""
        ...

    @property
    def is_closed(self) -> bool:
        """Whether the window has closed — by the user's gesture or this
        owner's own `close()`.

        Reflects what the last answered call reported, so it needs no round
        trip of its own; `drain_events()` and `show()` keep it current.
        """
        ...

    def show(
        self,
        frame_or_surface_to_show: ClaimedSurfacePixelAccess | GpuSurfaceHandle | str,
    ) -> None:
        """Name the frame this window shows next.

        Takes anything that names a published surface: a cast object read with
        `ctx.inputs.read(port, into=T)` — whose claim is what guarantees the id
        un-recycled — a `GpuSurfaceHandle` a kernel wrote, or a bare surface id.
        Returns without waiting for the frame to be shown: the window presents
        at vsync, latest-wins, and naming nothing leaves the last frame up.

        A bare id names a **texture-backed** surface only, and so does a cast
        type declaring no `width`/`height`: naming no extent is how a caller
        says it knows nothing else about the surface, and the engine reads that
        as refusing a buffer-backed one. Such a frame does not draw — the
        window keeps what it last had, and the engine logs it once per pool
        slot rather than raising here. A camera or a test pattern publishes
        buffer-backed frames; name those with the cast object.

        A no-op once the window has closed, never an error — a user gesture
        does not take a pipeline down. The argument is still read, so a call
        that names no surface at all is refused whether the window is open or
        shut.
        """
        ...

    def drain_events(self) -> NodeOwnedWindowEvents:
        """Take this window's coalesced state.

        Polling is optional — an owner that never drains still presents; it
        only learns of a resize or a close from the next `show()`.
        """
        ...

    def close(self) -> None:
        """Close this window and release its present thread.

        Never an error for a window already closed, and never required: the
        engine closes what a node still owns at teardown.
        """
        ...

    def __repr__(self) -> str: ...


class NodeOwnedWindowEvents(Protocol):
    """The coalesced state one `drain_events()` took off a window."""

    @property
    def current_width_in_physical_pixels(self) -> int: ...
    @property
    def current_height_in_physical_pixels(self) -> int: ...
    @property
    def close_requested_by_user(self) -> bool:
        """Whether the user asked to close this window since the last drain.

        True exactly once per gesture — this drain reported it and cleared it.
        The engine has already closed the window by the time an owner reads
        this, so it is reacted to and never vetoed.
        """
        ...

    @property
    def window_is_closed(self) -> bool:
        """Whether the engine has closed this window. Sticky once true."""
        ...

    def __repr__(self) -> str: ...
