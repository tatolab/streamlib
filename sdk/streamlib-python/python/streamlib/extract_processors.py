# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — pkg-build subprocess CLI; emits the manifest JSON on stdout and usage/errors on stderr with no log pipeline installed

"""Import-and-enumerate processor extractor for a Python package directory.

The Python analogue of Rust's `streamlib_processor_extract`: derive a
package's `processors:` manifest section from code rather than a hand-authored
list. Where the Rust capability parses source without running it, here
extraction *is* import — every top-level module is imported, which runs the
`@processor` decorators, which register into
[`_processor_registry`][streamlib._processor_registry]; the registered set is
then emitted.

Once the pkg-build truth-flip lands, `streamlib pkg build` will invoke this
in a fresh subprocess (`python -m streamlib.extract_processors
<package_dir>`), read the JSON on stdout, and write the manifest
`processors:` section — the same shape the Rust extractor feeds the catalog.
Running in a fresh process guarantees an empty registry to start; the
in-process [`extract_processors_from_dir`][] entrypoint clears the registry
itself so it is safe to call repeatedly.

Discovery matches the Rust scan's `collect_rs_files` + sort: every top-level
`*.py` beside the `streamlib.yaml`, imported in sorted filename order. Modules
are deduplicated through `sys.modules`, so a processor imported transitively
by an earlier module registers exactly once. The emitted list is sorted by
joined schema-ident string so output is deterministic regardless of import
order.
"""

from __future__ import annotations

import importlib
import json
import sys
from pathlib import Path
from typing import List

from ._processor_registry import (
    RegisteredProcessor,
    clear_registered_processors,
    registered_processors,
)


class ProcessorExtractionError(RuntimeError):
    """Raised when a package directory cannot be scanned for processors."""


def extract_processors_from_dir(package_dir: Path) -> List[RegisteredProcessor]:
    """Import every top-level module under `package_dir` and enumerate processors.

    Returns the processors registered by `@processor` during import, sorted by
    joined schema-ident string. The registry is cleared first, so repeated
    calls in one process are isolated. `sys.modules` and `sys.path` are
    restored on exit.

    Raises:
        ProcessorExtractionError: if `package_dir` is not a directory.
    """
    package_dir = package_dir.resolve()
    if not package_dir.is_dir():
        raise ProcessorExtractionError(
            f"not a directory: {package_dir} — nothing to scan for processors"
        )

    py_files = sorted(
        p
        for p in package_dir.glob("*.py")
        if p.is_file() and p.name != "__init__.py"
    )
    module_names = [p.stem for p in py_files]

    clear_registered_processors()

    # Force a fresh import of every target module: stash any pre-existing
    # `sys.modules` entry so a transitive import inside the loop can't collide
    # with (or be shadowed by) a stale module of the same name, then restore
    # on exit. Deduplication is left to the import machinery — a module
    # imported transitively by an earlier file is cached and not re-run.
    stashed = {name: sys.modules.pop(name, None) for name in module_names}
    sys.path.insert(0, str(package_dir))
    try:
        for name in module_names:
            importlib.import_module(name)
        procs = list(registered_processors())
    finally:
        sys.path.remove(str(package_dir))
        for name in module_names:
            sys.modules.pop(name, None)
        for name, prev in stashed.items():
            if prev is not None:
                sys.modules[name] = prev

    procs.sort(key=lambda entry: str(entry.schema_ident))
    return procs


def _to_manifest_json(procs: List[RegisteredProcessor]) -> str:
    """Render extracted processors as the JSON `pkg build` consumes on stdout."""
    payload = [
        {
            "name": entry.short_name,
            "schema_ident": entry.schema_ident.to_wire_dict(),
            "execution": entry.execution,
            "scheduling": entry.scheduling,
            "description": entry.description,
            "inputs": [
                {
                    "name": port["name"],
                    "schema": (
                        port["schema"].to_wire_dict()
                        if port["schema"] is not None
                        else None
                    ),
                    "description": port["description"],
                    "delivery_profile": port.get("delivery_profile"),
                }
                for port in entry.inputs
            ],
            "outputs": [
                {
                    "name": port["name"],
                    "schema": (
                        port["schema"].to_wire_dict()
                        if port["schema"] is not None
                        else None
                    ),
                    "description": port["description"],
                }
                for port in entry.outputs
            ],
        }
        for entry in procs
    ]
    return json.dumps(payload, indent=2)


def main(argv: List[str]) -> int:
    """CLI entrypoint: `python -m streamlib.extract_processors <package_dir>`."""
    if len(argv) != 1:
        sys.stderr.write(
            "usage: python -m streamlib.extract_processors <package_dir>\n"
        )
        return 2
    try:
        procs = extract_processors_from_dir(Path(argv[0]))
    except ProcessorExtractionError as exc:
        sys.stderr.write(f"{exc}\n")
        return 1
    sys.stdout.write(_to_manifest_json(procs))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
