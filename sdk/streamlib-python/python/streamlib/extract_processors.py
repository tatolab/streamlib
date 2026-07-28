# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — pkg-build subprocess CLI; emits the manifest JSON on stdout and usage/errors on stderr with no log pipeline installed

"""Import-and-enumerate processor extractor for a Python package directory.

The Python analogue of Rust's `streamlib_processor_extract`: derive a
package's `processors:` manifest section from code rather than a hand-authored
list. Where the Rust capability parses source without running it, here
extraction *is* import — every processor module is imported, which runs the
`@processor` decorators, which register into
[`_processor_registry`][streamlib._processor_registry]; the registered set is
then emitted.

Once the truth-flip lands, `streamlib pkg publish` will invoke this
in a fresh subprocess (`python -m streamlib.extract_processors
<package_dir>`), read the JSON on stdout, and write the manifest
`processors:` section — the same shape the Rust extractor feeds the catalog.
Running in a fresh process guarantees an empty registry to start; the
in-process [`extract_processors_from_dir`][] entrypoint clears the registry
itself so it is safe to call repeatedly.

`processors/` is the discovery root, the polyglot analogue of the Rust
extractor's `src/`: every `*.py` under `<package_dir>/processors/`, walked
recursively. A `*.py` beside the `streamlib.yaml` is NOT a processor module,
and a package with no `processors/` directory yields no processors (a
schema-only package is legitimate). Test scaffolding is skipped —
`test_*.py`, `*_test.py`, `conftest.py`, and any `tests/` or `__tests__/`
directory, the same skip set the Deno extractor applies.

Each module is imported under its dotted path relative to `package_dir`
(`processors/blur.py` → `processors.blur`, `processors/vision/blur.py` →
`processors.vision.blur`), which is exactly the module half of the
`entrypoint:` a built manifest carries. `processors/` needs no `__init__.py`:
PEP 420 makes it a namespace package. An `__init__.py` is never imported
directly, so a `@processor` declared in one is discovered only incidentally
(when a sibling module's import executes it as an ancestor package) — declare
processors in a module, never in an `__init__.py`.

The root governs DISCOVERY, not registration: a `@processor` in a module
outside `processors/` still registers if a discovered module imports it, and
the per-call isolation guarantee covers only the enumerated modules and their
ancestor packages. Modules are deduplicated through `sys.modules`, so a
processor imported transitively by an earlier module registers exactly once.

Modules are imported in sorted path-segment order (matching the Deno
extractor's ordering, so both runtimes evaluate a nested tree in the same
sequence); the emitted list is then sorted by joined schema-ident codepoint
order, so output is deterministic regardless of import order.
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


#: The one directory, relative to the package root, processor modules are
#: discovered under. Mirrors the Rust extractor's `src/` root.
PROCESSOR_SOURCE_DIR_NAME = "processors"


#: Directory names under `processors/` that hold test scaffolding, never
#: processor modules. Mirrors the Deno extractor's directory skip set.
TEST_SCAFFOLDING_DIR_NAMES = ("tests", "__tests__")


def _is_extractable_processor_module_file(file_name: str) -> bool:
    """Whether a `*.py` under `processors/` is a module extraction should import."""
    if file_name in ("__init__.py", "conftest.py"):
        return False
    return not (file_name.startswith("test_") or file_name.endswith("_test.py"))


def _is_under_test_scaffolding_dir(relative_path: Path) -> bool:
    """Whether a `processors/`-relative path sits under a test-scaffolding dir."""
    return any(part in TEST_SCAFFOLDING_DIR_NAMES for part in relative_path.parts[:-1])


def _module_names_including_ancestor_packages(module_names: List[str]) -> List[str]:
    """Every dotted module name plus each of its ancestor package names.

    Importing `processors.vision.blur` also materialises `processors` and
    `processors.vision` in `sys.modules`; extraction must stash and restore
    those too. A PEP 420 namespace package would recover on its own — its
    `__path__` recomputes when `sys.path` changes — but a package that ships
    `processors/__init__.py` is a *regular* package whose `__path__` is a
    static list pinned at the first package directory, so a second extraction
    over a different directory would resolve `processors.*` against the first
    one's tree and raise `ModuleNotFoundError`.
    """
    names = set(module_names)
    for module_name in module_names:
        segments = module_name.split(".")
        for depth in range(1, len(segments)):
            names.add(".".join(segments[:depth]))
    return sorted(names)


def extract_processors_from_dir(package_dir: Path) -> List[RegisteredProcessor]:
    """Import every module under `<package_dir>/processors/` and enumerate processors.

    Returns the processors registered by `@processor` during import, sorted by
    joined schema-ident string. The registry is cleared first, so repeated
    calls in one process are isolated. `sys.modules` and `sys.path` are
    restored on exit. A package with no `processors/` directory yields `[]` —
    a schema-only package declares no processors.

    Raises:
        ProcessorExtractionError: if `package_dir` is not a directory.
    """
    package_dir = package_dir.resolve()
    if not package_dir.is_dir():
        raise ProcessorExtractionError(
            f"not a directory: {package_dir} — nothing to scan for processors"
        )

    clear_registered_processors()

    processor_source_dir = package_dir / PROCESSOR_SOURCE_DIR_NAME
    if not processor_source_dir.is_dir():
        return []

    py_files = sorted(
        (
            p
            for p in processor_source_dir.rglob("*.py")
            if p.is_file()
            and _is_extractable_processor_module_file(p.name)
            and not _is_under_test_scaffolding_dir(p.relative_to(processor_source_dir))
        ),
        key=lambda p: p.relative_to(package_dir).parts,
    )
    # The dotted path relative to the package root is both the import name
    # (`package_dir` is on `sys.path`) and the module half of the manifest
    # `entrypoint:` — full-relative naming keeps nested modules collision-free.
    module_names = [
        ".".join(p.relative_to(package_dir).with_suffix("").parts) for p in py_files
    ]

    # Force a fresh import of every target module: stash any pre-existing
    # `sys.modules` entry so a transitive import inside the loop can't collide
    # with (or be shadowed by) a stale module of the same name, then restore
    # on exit. Deduplication is left to the import machinery — a module
    # imported transitively by an earlier file is cached and not re-run.
    stash_names = _module_names_including_ancestor_packages(module_names)
    stashed = {name: sys.modules.pop(name, None) for name in stash_names}
    sys.path.insert(0, str(package_dir))
    try:
        for name in module_names:
            importlib.import_module(name)
        procs = list(registered_processors())
    finally:
        sys.path.remove(str(package_dir))
        for name in stash_names:
            sys.modules.pop(name, None)
        for name, prev in stashed.items():
            if prev is not None:
                sys.modules[name] = prev

    procs.sort(key=lambda entry: str(entry.schema_ident))
    return procs


def _to_manifest_json(procs: List[RegisteredProcessor]) -> str:
    """Render extracted processors as the JSON `pkg publish` consumes on stdout."""
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
