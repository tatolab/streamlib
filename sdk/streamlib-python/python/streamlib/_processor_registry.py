# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Module-level registry the `@processor` decorator appends to.

The decorator is the manifest truth-source: applying `@processor(...)` at
import time registers the processor's structured identity here, so a
downstream extractor can enumerate a package's processors by *importing* its
modules and reading this registry — never by trusting a hand-authored
`processors:` list in `streamlib.yaml`. This is the Python analogue of the
Rust `syn` source-scan in `sdk/streamlib-processor-extract`; there the scan
reads the AST without running it, here extraction *is* import.

The registry is process-global and append-only during a normal import. An
extractor that wants a package's processors in isolation calls
[`clear_registered_processors`][] before importing that package's modules.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import List, Tuple

from .schema_ident import SchemaIdent


@dataclass(frozen=True)
class RegisteredProcessor:
    """One processor derived from a `@processor(...)` decorator at import time.

    Mirrors Rust's `streamlib_processor_extract::ExtractedProcessor`: the
    structured identity the manifest/`.slpkg` assembly consumes, plus the
    port metadata declared by `@input` / `@output` and the class it was
    written on (for diagnostics).
    """

    short_name: str
    schema_ident: SchemaIdent
    inputs: Tuple[dict, ...] = field(default=())
    outputs: Tuple[dict, ...] = field(default=())
    class_qualname: str = ""


_REGISTERED_PROCESSORS: List[RegisteredProcessor] = []


def register_processor(entry: RegisteredProcessor) -> None:
    """Append a decorator-derived processor to the process-global registry."""
    _REGISTERED_PROCESSORS.append(entry)


def registered_processors() -> Tuple[RegisteredProcessor, ...]:
    """Snapshot the processors registered so far, in registration order."""
    return tuple(_REGISTERED_PROCESSORS)


def clear_registered_processors() -> None:
    """Empty the registry.

    Used by the import-and-enumerate extractor to isolate one package's
    processors from anything already imported in the same process.
    """
    _REGISTERED_PROCESSORS.clear()
