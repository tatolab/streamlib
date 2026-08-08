# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Discovering the StreamLib nodes running on this machine.

A node that hosts a control plane writes one JSON file per live node into the
OS's per-user runtime directory. This reads that registry, liveness-checks each
entry, and prunes the ones that are definitively gone.

Liveness has two independent signals: whether the control plane answers, and
whether the host process still exists. An entry is deleted only when BOTH say
dead, so a live node that is briefly slow to answer is never pruned out from
under its own process.
"""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import NamedTuple, Optional

__all__ = [
    "NodeRegistryEntry",
    "DiscoveredNode",
    "registry_directory",
    "scan_check_and_prune",
    "live_nodes",
]

NODE_REGISTRY_SCHEMA_VERSION = 1


class NodeRegistryEntry(NamedTuple):
    """One node's discovery record, as written by its control plane."""

    schema_version: int
    runtime_id: str
    control_url: str
    pid: int
    hint: str


class DiscoveredNode(NamedTuple):
    """A registry entry paired with its liveness verdict."""

    entry: NodeRegistryEntry
    #: The control plane answered a round-trip. This is the column that matters
    #: to a control verb — a node whose process is alive but whose control plane
    #: is silent cannot be driven.
    reachable: bool


def registry_directory() -> Path:
    """Where control-plane-hosting runtimes publish their discovery entries.

    Mirrors the engine's own resolution: `$XDG_RUNTIME_DIR/streamlib/nodes`,
    falling back to the system temp dir when the variable is unset.
    """
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR")
    if runtime_dir:
        return Path(runtime_dir) / "streamlib" / "nodes"
    return Path(tempfile.gettempdir()) / "streamlib" / "nodes"


def _read_entry_file(path: Path) -> "Optional[NodeRegistryEntry]":
    """Parse one registry file, or `None` if it is unreadable or malformed.

    A half-written or hand-edited file is skipped rather than raised on: the
    registry is a directory of independent files, and one bad entry must not
    make every other node undiscoverable.

    An entry whose `schema_version` this reader does not know is skipped for the
    same reason the field exists — and skipping it here is what keeps it OUT of
    the prune path, so a reader never deletes a record it cannot parse.
    """
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
        if int(record["schema_version"]) != NODE_REGISTRY_SCHEMA_VERSION:
            return None
        return NodeRegistryEntry(
            schema_version=int(record["schema_version"]),
            runtime_id=str(record["runtime_id"]),
            control_url=str(record["control_url"]),
            pid=int(record["pid"]),
            hint=str(record.get("hint", "")),
        )
    except (OSError, ValueError, KeyError, TypeError):
        return None


def _process_exists(pid: int) -> bool:
    """Whether a process with `pid` currently exists.

    `kill(pid, 0)` delivers no signal — it only performs the permission and
    existence checks. `PermissionError` means the process is there but not ours
    to signal, which is still alive.

    A pid outside `pid_t` raises `OverflowError`, which is not an `OSError` — so
    it is caught by name. A corrupt entry carrying one would otherwise crash the
    whole scan and make every other node undiscoverable.
    """
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OSError, OverflowError, ValueError):
        return False
    return True


def scan_check_and_prune() -> "list[DiscoveredNode]":
    """Every registry entry, liveness-checked, with the dead ones deleted.

    Entries that are unreachable but whose process is alive are returned with
    `reachable=False` rather than pruned.
    """
    # Imported here rather than at module scope: the client imports this module
    # for the URL resolver, and a module-level cycle would break either import
    # depending on which the CLI reached first.
    from ._control_plane_client import control_plane_answers

    directory = registry_directory()
    try:
        entry_files = sorted(directory.glob("*.json"))
    except OSError:
        return []

    discovered: "list[DiscoveredNode]" = []
    for entry_file in entry_files:
        entry = _read_entry_file(entry_file)
        if entry is None:
            continue
        reachable = control_plane_answers(entry.control_url)
        if not reachable and not _process_exists(entry.pid):
            try:
                entry_file.unlink()
            except OSError:
                # Another process pruning the same stale entry is the expected
                # race, and it reached the same verdict we did.
                pass
            continue
        discovered.append(DiscoveredNode(entry=entry, reachable=reachable))
    return discovered


def live_nodes() -> "list[NodeRegistryEntry]":
    """The reachable nodes only, with dead entries already pruned."""
    return [node.entry for node in scan_check_and_prune() if node.reachable]
