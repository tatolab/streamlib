# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decorators for defining StreamLib processors and schemas in Python.

`@processor("PascalCase")` mirrors Rust's `#[streamlib::processor("Camera")]`
proc-macro: a positional PascalCase short name that the decorator resolves
against the sibling `streamlib.yaml`'s `package: { org, name, version }`
block at decoration time. The result is a structured
[`SchemaIdent`][streamlib.schema_ident.SchemaIdent] attached to the class
as `__streamlib_schema_ident__`.

Schema references in port declarations (`@input(schema=...)` /
`@output(schema=...)`) are cross-package by definition. The only accepted
forms are a `SchemaIdent` instance or a `@schema`-decorated class. Bare
type names and joined-string identifiers are rejected — see the
architecture preamble in issue #700 and
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
import sys
from pathlib import Path
from typing import List, Optional, Type, Union

from ._manifest import ManifestParseError, read_manifest_summary
from .schema_ident import SchemaIdent


# =============================================================================
# Schema Field Descriptors
# =============================================================================


class SchemaField:
    """Descriptor for a field in a schema.

    Used by the field descriptor functions (f32, i64, etc.) to define
    schema fields. The @schema decorator collects these into a
    Rust-backed DynamicDataFrameSchema.
    """

    def __init__(
        self,
        primitive_type: str,
        shape: Optional[List[int]] = None,
        description: str = "",
    ):
        self.primitive_type = primitive_type
        self.shape = shape or []
        self.description = description

    def __repr__(self) -> str:
        if self.shape:
            return f"SchemaField({self.primitive_type}, shape={self.shape})"
        return f"SchemaField({self.primitive_type})"


def f32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit float field."""
    return SchemaField("f32", shape, description)


def f64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit float field."""
    return SchemaField("f64", shape, description)


def i32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit signed integer field."""
    return SchemaField("i32", shape, description)


def i64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit signed integer field."""
    return SchemaField("i64", shape, description)


def u32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit unsigned integer field."""
    return SchemaField("u32", shape, description)


def u64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit unsigned integer field."""
    return SchemaField("u64", shape, description)


def bool_field(description: str = "") -> SchemaField:
    """Define a boolean field.

    Named ``bool_field`` to avoid shadowing Python's ``bool`` builtin.
    """
    return SchemaField("bool", None, description)


# =============================================================================
# Schema Decorator (untouched here; structured-ident parity tracked in #704)
# =============================================================================


def schema(name: Optional[str] = None):
    """Define a custom data schema backed by Rust.

    The decorated class should have class attributes that are SchemaField
    instances (created via f32, i64, bool_field, etc.). The decorator
    collects these fields and creates a Rust-backed DynamicDataFrameSchema.

    NOTE: The structured-`SchemaIdent` parity migration for `@schema`
    tracks in issue #704 (filed alongside #700). This decorator's shape
    is unchanged in #700 — it still attaches a `__streamlib_schema__` dict
    carrier; #704 replaces that with `__streamlib_schema_ident__: SchemaIdent`.
    """

    def decorator(cls):
        schema_name = name or cls.__name__

        fields = []
        for attr_name in dir(cls):
            if attr_name.startswith("_"):
                continue
            attr_value = getattr(cls, attr_name, None)
            if isinstance(attr_value, SchemaField):
                fields.append(
                    {
                        "name": attr_name,
                        "primitive_type": attr_value.primitive_type,
                        "shape": attr_value.shape,
                        "description": attr_value.description,
                    }
                )

        cls.__streamlib_schema__ = {
            "name": schema_name,
            "fields": fields,
        }

        return cls

    return decorator


# =============================================================================
# Processor Decorator
# =============================================================================


def processor(short_name: str):
    """Mark a class as a StreamLib processor (PascalCase positional short name).

    Mirrors Rust's `#[streamlib::processor("Camera")]` macro. At decoration
    time, the decorator locates the sibling `streamlib.yaml` (next to the
    file containing the decorated class), reads its
    `package: { org, name, version }` block, validates that `short_name`
    appears in the manifest's `processors:` list, and constructs a
    structured `SchemaIdent` attached to the class as
    `__streamlib_schema_ident__`.

    Args:
        short_name: PascalCase type name. Must match an entry in the
            sibling manifest's `processors:` list.

    Raises:
        FileNotFoundError: if no sibling `streamlib.yaml` is found.
        ManifestParseError: if the manifest is malformed or missing
            required `package:` fields.
        ValueError: if `short_name` is not declared in the manifest.

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
            summary = read_manifest_summary(manifest_path)
        except ManifestParseError:
            raise
        except FileNotFoundError as exc:
            raise FileNotFoundError(
                f"streamlib.yaml not found at {manifest_path}. "
                f"@processor({short_name!r}) requires a sibling streamlib.yaml "
                f"with a `package: {{ org, name, version }}` block and a matching "
                f"`processors:` entry."
            ) from exc

        if short_name not in summary.processor_names:
            available = "\n    ".join(summary.processor_names) or "(none declared)"
            raise ValueError(
                f"@processor({short_name!r}): short name not declared in "
                f"{manifest_path}'s `processors:` list. Available processors:\n    "
                f"{available}"
            )

        ident = SchemaIdent(
            org=summary.package.org,
            package=summary.package.name,
            type_=short_name,
            version=summary.package.version,
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

        return cls

    return decorator


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
        schema: A structured carrier — either a `SchemaIdent` instance
            (cross-package or same-package) or a class decorated with
            `@schema` (whose `__streamlib_schema_ident__` is read).
            String forms (bare type name or joined `@org/pkg/Type@v`)
            are rejected with a clear error.
        description: Human-readable description for introspection.

    Example:
        ```python
        from streamlib import input, SchemaIdent

        VIDEO_FRAME = SchemaIdent("tatolab", "core", "VideoFrame", "1.0.0")

        @input(schema=VIDEO_FRAME, description="RGB video input")
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
        - a class with `__streamlib_schema_ident__` attribute (a
          `@schema`-decorated class once #704 lands; today these classes
          carry `__streamlib_schema__` legacy dict — that path is rejected
          with guidance until #704)

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
        if hasattr(schema_arg, "__streamlib_schema__"):
            raise TypeError(
                f"schema={schema_arg.__name__}: this class is decorated with the "
                f"legacy `@schema(name=...)` form whose structured-ident parity "
                f"lands in #704. Pass a `SchemaIdent` instance directly until then."
            )
        raise TypeError(
            f"schema={schema_arg.__name__}: class does not carry a structured "
            f"SchemaIdent. Decorate it with `@schema(\"PascalCase\")` (post-#704) "
            f"or pass a `SchemaIdent` instance directly."
        )
    raise TypeError(
        f"schema={schema_arg!r}: unsupported type {type(schema_arg).__name__}. "
        f"Pass a `SchemaIdent` instance or a `@schema`-decorated class."
    )
