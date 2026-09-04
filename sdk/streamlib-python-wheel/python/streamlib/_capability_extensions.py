# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Running the installed capability extensions' hooks, once per process.

A wheel declares `[project.entry-points."streamlib.extensions"] <name> =
"<module>:load"`; pip records that at install, and this reads pip's registry
through `importlib.metadata` rather than scanning for files. Every hook runs in
every process that takes an engine role — the app process as `Runtime()` is
constructed, a helper before the processor's own module is imported — so a
stack a hook brings up exists wherever the wheel's processors run.

A hook that raises stops the process it was loading into. Skipping it would
leave an extension half loaded, which is worse than one that refused: its
processors would then fail per frame, far from the cause.
"""

from __future__ import annotations

import importlib.metadata
from typing import Callable, Protocol


class CapabilityExtensionHostFactory(Protocol):
    """Mints the host one distribution's hook is handed."""

    def __call__(self, distribution: str, /) -> object: ...


class CapabilityExtensionLoadError(RuntimeError):
    """A capability extension's `load(host)` hook did not complete."""


#: Set once the loop has run to completion in this process.
_HOOKS_HAVE_RUN = False

#: The failure the loop ended on, re-raised rather than re-attempted.
_HOOK_FAILURE: "CapabilityExtensionLoadError | None" = None


def run_every_installed_capability_extension_hook(
    mint_host_for_distribution: CapabilityExtensionHostFactory,
) -> None:
    """Call `load(host)` on every `streamlib.extensions` entry point installed.

    Raises `CapabilityExtensionLoadError` naming the distribution and the entry
    point at the first hook that fails, without running the rest.
    """
    for entry_point in importlib.metadata.entry_points(group="streamlib.extensions"):
        distribution = _distribution_name_of(entry_point)
        try:
            load: Callable[[object], None] = entry_point.load()
            load(mint_host_for_distribution(distribution))
        except Exception as hook_failure:
            raise CapabilityExtensionLoadError(
                f"the capability extension `{entry_point.name}` from `{distribution}` "
                f"failed to load: {hook_failure}"
            ) from hook_failure


def load_installed_capability_extensions_once_per_process(
    mint_host_for_distribution: CapabilityExtensionHostFactory,
) -> None:
    """Run the loop the first time only, and re-raise its failure after that.

    A second `Runtime()` in one process re-runs nothing: a hook brings a stack
    up, and bringing it up twice is what the once-per-process contract exists
    to prevent.
    """
    global _HOOKS_HAVE_RUN, _HOOK_FAILURE
    if _HOOK_FAILURE is not None:
        raise _HOOK_FAILURE
    if _HOOKS_HAVE_RUN:
        return
    try:
        run_every_installed_capability_extension_hook(mint_host_for_distribution)
    except CapabilityExtensionLoadError as hook_failure:
        _HOOK_FAILURE = hook_failure
        raise
    _HOOKS_HAVE_RUN = True


def _distribution_name_of(entry_point: importlib.metadata.EntryPoint) -> str:
    """Which installed distribution declared `entry_point`.

    `dist` is set by the discovery that produced the entry point, so it is
    populated for anything `entry_points()` returns; the fallback exists so a
    refusal reads as a refusal rather than an AttributeError.
    """
    distribution = getattr(entry_point, "dist", None)
    return "an unknown distribution" if distribution is None else distribution.name
