# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decorators for defining StreamLib processors in Python.

`@processor("PascalCase")` mirrors Rust's `#[streamlib::processor("Camera")]`
proc-macro: a positional PascalCase short name. The decorator reads the
package identity (`package: { org, name, version }`) from the sibling
`streamlib.yaml`, composes a structured
[`SchemaIdent`][streamlib.schema_ident.SchemaIdent] attached to the class as
`__streamlib_schema_ident__`, and registers the processor in the
process-global [`_processor_registry`][streamlib._processor_registry].

The decorator is the manifest truth-source for the `processors:` set: a
package's processors are derived by *importing* its modules and enumerating
what `@processor` registered — never by reading a hand-authored `processors:`
list. This is the Python analogue of the Rust `syn` source-scan in
`sdk/streamlib-processor-extract` (there the scan reads the AST without
running it; here extraction is import). Only the package identity comes from
`streamlib.yaml`; the processor set comes from code. See
[`streamlib.extract_processors`][].

Schema references in port declarations (`@input(schema=...)` /
`@output(schema=...)`) are cross-package by definition. The only accepted
forms are a [`SchemaIdent`][streamlib.schema_ident.SchemaIdent] instance
or a codegen-emitted class carrying `__streamlib_schema_ident__` as a
class attribute (produced by `streamlib generate` from the package's
JTD/YAML schemas). There is no Python-side authoring decorator for
declaring new schemas — JTD-in-YAML is the canonical schema source, and
deriving JTD from Python field declarations would leak Python-native
expressivity that doesn't translate cross-language. See the architecture
preamble in issue #704 and
`docs/architecture/schema-identity-and-packaging.md`.

Timestamps
----------

For any timestamp that crosses the host/subprocess boundary or is
compared against another runtime's stamps — frame stamps, log
correlation, escalate request IDs, anything similar — use
``streamlib.monotonic_now_ns()``. It calls
``clock_gettime(CLOCK_MONOTONIC)``, the same kernel syscall the host
Rust runtime and the Deno SDK make, so values share a system-wide
epoch and are directly comparable.

Do NOT use ``time.time()``, ``datetime.now()``, or ``time.time_ns()``
for cross-process timestamps — wall-clock APIs drift under NTP and
reflect different epochs from one process to the next. Wall-clock
APIs are still appropriate for ISO8601 formatting and other genuinely
human-facing display.
"""

from __future__ import annotations

import inspect
import re
from pathlib import Path
from typing import Optional, Pattern, Type, Union

from ._manifest import ManifestParseError, read_package_block
from ._processor_registry import RegisteredProcessor, register_processor
from .schema_ident import SchemaIdent


# =============================================================================
# Processor Decorator
# =============================================================================


def processor(short_name: str):
    """Mark a class as a StreamLib processor (PascalCase positional short name).

    Mirrors Rust's `#[streamlib::processor("Camera")]` macro. At decoration
    time, the decorator locates the sibling `streamlib.yaml` (next to the
    file containing the decorated class), reads its
    `package: { org, name, version }` block for the package identity,
    constructs a structured `SchemaIdent` attached to the class as
    `__streamlib_schema_ident__`, and registers the processor in the
    process-global registry so the import-and-enumerate extractor can derive
    the package's `processors:` set from code. The `short_name` is NOT
    validated against a `processors:` list in the manifest — the decorator IS
    that list.

    Args:
        short_name: PascalCase type name — the processor's identity.

    Raises:
        FileNotFoundError: if no sibling `streamlib.yaml` is found.
        ManifestParseError: if the manifest is malformed or missing
            required `package:` fields.

    Example:
        ```python
        from streamlib import processor

        @processor("CyberpunkProcessor")
        class CyberpunkProcessor:
            ...
        ```

    Wire format and IPC always carry the full structured `SchemaIdent`;
    the short-name positional is an authoring convenience for the
    processor's own identity declaration only — schema references in
    port declarations (`@input(schema=...)` / `@output(schema=...)`)
    have no analogous shorthand and require a structured carrier.
    """
    if not isinstance(short_name, str):
        raise TypeError(
            f"@processor() takes a positional PascalCase short name (str); "
            f"got {type(short_name).__name__}. The legacy `name=`/`description=`/"
            f"`execution=` kwarg form is removed (pre-1.0 policy)."
        )

    def decorator(cls):
        manifest_path = _locate_sibling_manifest(cls)
        try:
            package = read_package_block(manifest_path)
        except ManifestParseError:
            raise
        except FileNotFoundError as exc:
            raise FileNotFoundError(
                f"streamlib.yaml not found at {manifest_path}. "
                f"@processor({short_name!r}) requires a sibling streamlib.yaml "
                f"with a `package: {{ org, name, version }}` block."
            ) from exc

        # Schema idents are release-only by invariant: a package may carry a
        # `-dev.N` / `-rc.N` prerelease version, but its schema idents project
        # onto the release core (mirrors Rust's `SemVer::release_core`). The
        # 3-part `SchemaIdent` validator would otherwise reject a legitimately
        # dev-versioned package's processors.
        ident = SchemaIdent(
            org=package.org,
            package=package.name,
            type_=short_name,
            version=_release_core(package.version),
        )
        cls.__streamlib_schema_ident__ = ident

        # Collect port metadata declared by @input / @output for runtime
        # introspection. Port schemas are already SchemaIdent instances at
        # this point (see _resolve_schema_ident below).
        inputs = []
        outputs = []
        for attr_name in dir(cls):
            attr = getattr(cls, attr_name, None)
            if callable(attr):
                if hasattr(attr, "_streamlib_input_port"):
                    inputs.append(attr._streamlib_input_port)
                if hasattr(attr, "_streamlib_output_port"):
                    outputs.append(attr._streamlib_output_port)
        cls.__streamlib_ports__ = {"inputs": inputs, "outputs": outputs}

        register_processor(
            RegisteredProcessor(
                short_name=short_name,
                schema_ident=ident,
                inputs=tuple(inputs),
                outputs=tuple(outputs),
                class_qualname=getattr(cls, "__qualname__", cls.__name__),
            )
        )

        return cls

    return decorator


# Package-version grammar: 3-part core + optional closed `-dev.N` / `-rc.N`
# prerelease. Mirrors Rust's `SemVer::from_dotted` so all three runtimes
# accept and reject the same manifests.
_PACKAGE_VERSION_PATTERN: Pattern[str] = re.compile(
    r"^(\d+\.\d+\.\d+)(?:-(?:dev|rc)\.\d+)?$"
)


def _release_core(version: str) -> str:
    """Project a package version onto its release core `MAJOR.MINOR.PATCH`.

    Package versions may carry a `-dev.N` / `-rc.N` prerelease, but schema
    idents are release-only by invariant. Anything outside that closed
    grammar (`-alpha.1`, `+build`, malformed ordinals) raises — identical
    posture to Rust's manifest parsing, never a silent projection of an
    invalid version. Mirrors Rust's `streamlib_idents::SemVer::release_core`.
    """
    match = _PACKAGE_VERSION_PATTERN.match(version)
    if match is None:
        raise ValueError(
            f"invalid package version {version!r}: must be MAJOR.MINOR.PATCH "
            f"with an optional -dev.N / -rc.N prerelease"
        )
    return match.group(1)


def _locate_sibling_manifest(cls) -> Path:
    """Find the streamlib.yaml next to the file the class lives in.

    Looks at the directory containing the source file of `cls` for a
    `streamlib.yaml`. If not present there, returns that path anyway —
    the caller raises FileNotFoundError with the expected location, which
    is more useful than walking up arbitrarily.
    """
    try:
        source_file = inspect.getfile(cls)
    except TypeError as exc:
        # Built-in classes / dynamically-created classes without a source.
        raise TypeError(
            f"@processor cannot resolve a source file for {cls!r}; the "
            f"decorator must be applied to a class defined in a regular "
            f"Python module."
        ) from exc
    return Path(source_file).resolve().parent / "streamlib.yaml"


# =============================================================================
# Port Decorators
# =============================================================================


def input(
    name: Optional[str] = None,
    *,
    schema: Union[SchemaIdent, Type, None] = None,
    description: str = "",
):
    """Mark a method as defining an input port.

    Args:
        name: Port name. Defaults to the method name.
        schema: A structured carrier — either a `SchemaIdent` instance or
            a codegen-emitted class that carries `__streamlib_schema_ident__`
            as a class attribute (produced by `streamlib generate` from the
            package's JTD/YAML schemas). String forms (bare type name or
            joined `@org/pkg/Type@v`) are rejected with a clear error.
        description: Human-readable description for introspection.

    Example:
        ```python
        from streamlib._generated_.tatolab__core import VideoFrame

        @input(schema=VideoFrame, description="RGB video input")
        def video_in(self): pass
        ```
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_input_port = {
            "name": port_name,
            "schema": _resolve_schema_ident(schema),
            "description": description,
        }
        return method

    return decorator


def output(
    name: Optional[str] = None,
    *,
    schema: Union[SchemaIdent, Type, None] = None,
    description: str = "",
):
    """Mark a method as defining an output port.

    Same shape as [`input`][streamlib.decorators.input]; see that doc for
    the accepted `schema` carriers.
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_output_port = {
            "name": port_name,
            "schema": _resolve_schema_ident(schema),
            "description": description,
        }
        return method

    return decorator


def _resolve_schema_ident(schema_arg) -> Optional[SchemaIdent]:
    """Resolve a `schema=` argument to a structured `SchemaIdent`.

    Accepts:
        - `None` (port has no declared schema)
        - `SchemaIdent` instance (returned as-is)
        - a codegen-emitted class carrying `__streamlib_schema_ident__`
          as a `ClassVar[SchemaIdent]` attribute (produced by
          `streamlib generate` from the package's JTD/YAML schemas)

    Rejects:
        - any string (bare type name OR joined `@org/pkg/Type@v` form)
        - classes without structured-ident metadata
    """
    if schema_arg is None:
        return None
    if isinstance(schema_arg, SchemaIdent):
        return schema_arg
    if isinstance(schema_arg, str):
        raise TypeError(
            f"schema={schema_arg!r}: string schema references are no longer "
            f"accepted. Pass a structured `SchemaIdent(org, package, type_, version)` "
            f"instance instead. Joined-string forms like '@tatolab/core/VideoFrame@1.0.0' "
            f"and bare type names like 'VideoFrame' are both rejected — schemas are "
            f"cross-package references by definition and have no shorthand. See "
            f"docs/architecture/schema-identity-and-packaging.md."
        )
    if isinstance(schema_arg, type):
        ident = getattr(schema_arg, "__streamlib_schema_ident__", None)
        if isinstance(ident, SchemaIdent):
            return ident
        raise TypeError(
            f"schema={schema_arg.__name__}: class does not carry a structured "
            f"SchemaIdent. Import a codegen-emitted class from "
            f"streamlib._generated_.<package>, or pass a `SchemaIdent` "
            f"instance directly."
        )
    raise TypeError(
        f"schema={schema_arg!r}: unsupported type {type(schema_arg).__name__}. "
        f"Pass a `SchemaIdent` instance or a codegen-emitted schema class."
    )
