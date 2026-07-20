# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decorators for defining StreamLib processors in Python.

`@processor("@org/package/Type", execution=...)` mirrors Rust's
`#[processor("@org/package/Type", execution = ...)]` proc-macro
(`sdk/streamlib-processor-extract/src/grammar.rs`): identity, execution
mode, and scheduling are declared **in code**, never read from a sibling
`streamlib.yaml` at decoration time. The identity string is **version-free**
(`@org/package/Type`, no `@version`) — a schema ref is an identity the runtime
binds version-blind, and the concrete version is derived at package-build time,
never hand-authored (#1409). The decorator synthesizes the `SemVer 0.0.0`
version-free sentinel and attaches a structured
[`SchemaIdent`][streamlib.schema_ident.SchemaIdent] to the class as
`__streamlib_schema_ident__`, registering the processor in the process-global
[`_processor_registry`][streamlib._processor_registry].

Omitting the identity synthesizes `@app/local/<ClassName>` — a bare `.py`
module with no `streamlib.yaml` defines a working local processor.

The decorator is the manifest truth-source for the `processors:` set: a
package's processors are derived by *importing* its modules and enumerating
what `@processor` registered — never by reading a hand-authored `processors:`
list, and never by reading `package:` identity out of `streamlib.yaml`. This is
the Python analogue of the Rust `syn` source-scan in
`sdk/streamlib-processor-extract` (there the scan reads the AST without running
it; here extraction is import). See
[`streamlib.extract_processors`][].

Schema references in port declarations follow the two-door descriptor
model (`docs/architecture/zero-ceremony-authoring.md`). A port needs
**no** schema to move data: the wire is self-describing (msgpack named
maps / `Bag`), so send and receive work with zero type. When a port
*does* declare a schema — for validation, the visual builder, or opt-in
typed views — the reference is cross-package by definition, and the only
accepted forms are a [`SchemaIdent`][streamlib.schema_ident.SchemaIdent]
instance or a codegen-emitted class carrying `__streamlib_schema_ident__`
as a class attribute (produced by the opt-in `streamlib generate` from
the package's JTD/YAML schemas). There is still no Python-side decorator
for *authoring* a new schema: `streamlib generate` typed views are sugar
consumed as data, JTD-in-YAML remains the authored source for a shared
vocabulary type, and deriving JTD from Python field declarations would
leak Python-native expressivity that doesn't translate cross-language.
See `docs/architecture/schema-identity-and-packaging.md`.

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

import re
from typing import Optional, Pattern, Type, Union

from ._processor_registry import RegisteredProcessor, register_processor
from .schema_ident import SchemaIdent

# The version-free sentinel every code-declared identity carries. The concrete
# release version is derived at package-build time (#1409); the runtime schema
# registry stores and looks up unversioned, so `0.0.0` is an inert placeholder.
_VERSION_FREE_SENTINEL = "0.0.0"

_EXECUTION_MODES = ("reactive", "manual", "continuous")
_SCHEDULING_PRIORITIES = ("realtime", "high", "normal")


# =============================================================================
# Processor Decorator
# =============================================================================


def processor(
    identity: Optional[str] = None,
    *,
    execution: str,
    interval_ms: int = 0,
    scheduling: Optional[str] = None,
    description: Optional[str] = None,
):
    """Mark a class as a StreamLib processor — identity and mode declared in code.

    Mirrors Rust's `#[processor("@org/package/Type", execution = ...)]` macro.
    Nothing is read from disk: the version-free `@org/package/Type` identity, the
    execution mode, and the scheduling priority all come from the arguments. The
    decorator synthesizes the `0.0.0` version-free sentinel, attaches a structured
    `SchemaIdent` as `__streamlib_schema_ident__`, and registers the processor in
    the process-global registry so the import-and-enumerate extractor can derive
    the package's `processors:` set from code.

    Args:
        identity: Version-free `@org/package/Type` string. Omit to synthesize
            `@app/local/<ClassName>` — a bare module with no `streamlib.yaml`
            still defines a working local processor.
        execution: `"reactive"`, `"manual"`, or `"continuous"`. Required — the
            execution mode is authored in code, mirroring the Rust grammar.
        interval_ms: Minimum interval between `process()` calls, only meaningful
            for `execution="continuous"`.
        scheduling: `"realtime"`, `"high"`, or `"normal"`; omit for the default.
        description: Human-readable processor description for introspection.

    Raises:
        ValueError: if `identity` is a malformed or versioned identity string,
            if `execution` is not a known mode, if `scheduling` is not a known
            priority, or if an omitted identity cannot synthesize a valid
            `@app/local` type from the class name.

    Example:
        ```python
        from streamlib import processor

        @processor("@tatolab/camera/Camera", execution="manual", scheduling="high")
        class Camera:
            ...

        @processor(execution="reactive")  # → @app/local/LocalFilter
        class LocalFilter:
            ...
        ```
    """
    if identity is not None and not isinstance(identity, str):
        raise TypeError(
            f"@processor() identity must be a version-free `@org/package/Type` "
            f"string or omitted (for `@app/local/<ClassName>`); got "
            f"{type(identity).__name__}."
        )
    execution_spec = _normalize_execution(execution, interval_ms)
    scheduling_spec = _normalize_scheduling(scheduling)

    def decorator(cls):
        ident = _resolve_processor_identity(identity, cls)
        cls.__streamlib_schema_ident__ = ident
        cls.__streamlib_execution__ = execution_spec

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
                short_name=ident.type_,
                schema_ident=ident,
                execution=execution_spec,
                scheduling=scheduling_spec,
                description=description,
                inputs=tuple(inputs),
                outputs=tuple(outputs),
                class_qualname=getattr(cls, "__qualname__", cls.__name__),
            )
        )

        return cls

    return decorator


# Version-free identity grammar: `@<org>/<package>/<Type>`, no trailing
# `@<version>`. Mirrors Rust's `parse_schema_ident_str` in
# `sdk/streamlib-processor-extract/src/grammar.rs`.
_IDENTITY_PATTERN: Pattern[str] = re.compile(r"^@([^/@]+)/([^/@]+)/([^/@]+)$")


def _resolve_processor_identity(identity: Optional[str], cls) -> SchemaIdent:
    """Resolve the declared identity, or synthesize `@app/local/<ClassName>`."""
    if identity is None:
        type_name = getattr(cls, "__name__", None)
        try:
            return SchemaIdent(
                org="app",
                package="local",
                type_=type_name,
                version=_VERSION_FREE_SENTINEL,
            )
        except ValueError as exc:
            raise ValueError(
                f"cannot synthesize an `@app/local` identity for {cls!r}: "
                f"{exc}. Declare an explicit `@org/package/Type` identity, or "
                f"give the class a PascalCase name."
            ) from exc
    return _parse_identity_str(identity)


def _parse_identity_str(raw: str) -> SchemaIdent:
    """Parse a version-free `@org/package/Type` string into a `SchemaIdent`.

    The grammar is version-free (#1409): a trailing `@<version>` is rejected —
    a schema ref is an identity the runtime binds version-blind, and versions
    are derived at package-build time. The synthesized `SchemaIdent` carries
    the `0.0.0` version-free sentinel. Mirrors Rust's `parse_schema_ident_str`.
    """
    if not raw.startswith("@"):
        raise ValueError(
            f"schema identity {raw!r} must start with `@` "
            f"(e.g. `@tatolab/core/VideoFrame`)"
        )
    if "@" in raw[1:]:
        raise ValueError(
            f"schema identity {raw!r} must be version-free "
            f"`@<org>/<package>/<Type>` with no `@<version>` — a schema ref is "
            f"an identity the runtime binds version-blind; versions are derived "
            f"at package-build time, never hand-authored (#1409)"
        )
    match = _IDENTITY_PATTERN.match(raw)
    if match is None:
        raise ValueError(
            f"schema identity {raw!r} must be `@<org>/<package>/<Type>` "
            f"(exactly three `/`-separated segments)"
        )
    org, package, type_ = match.groups()
    return SchemaIdent(
        org=org,
        package=package,
        type_=type_,
        version=_VERSION_FREE_SENTINEL,
    )


def _normalize_execution(execution: str, interval_ms: int) -> Union[str, dict]:
    """Project the `execution=` / `interval_ms=` args onto the manifest shape.

    `reactive` / `manual` render as bare strings; `continuous` renders as the
    `{ "type": "continuous", "interval_ms": N }` mapping the Rust
    `ProcessorSchemaExecution` serializer emits.
    """
    if not isinstance(execution, str) or execution not in _EXECUTION_MODES:
        raise ValueError(
            f"invalid execution {execution!r}: must be one of "
            f"{', '.join(_EXECUTION_MODES)}"
        )
    if execution == "continuous":
        if (
            not isinstance(interval_ms, int)
            or isinstance(interval_ms, bool)
            or interval_ms < 0
        ):
            raise ValueError(
                f"invalid interval_ms {interval_ms!r}: must be a non-negative int"
            )
        return {"type": "continuous", "interval_ms": interval_ms}
    return execution


def _normalize_scheduling(scheduling: Optional[str]) -> Optional[dict]:
    """Project the `scheduling=` arg onto the manifest `{ priority }` shape."""
    if scheduling is None:
        return None
    if not isinstance(scheduling, str) or scheduling not in _SCHEDULING_PRIORITIES:
        raise ValueError(
            f"invalid scheduling {scheduling!r}: must be one of "
            f"{', '.join(_SCHEDULING_PRIORITIES)}"
        )
    return {"priority": scheduling}


# =============================================================================
# Port Decorators
# =============================================================================


def input(
    name: Optional[str] = None,
    *,
    schema: Union[SchemaIdent, Type, None] = None,
    description: str = "",
    delivery_profile: Optional[str] = None,
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
        delivery_profile: The one delivery knob — `"latest"`, `"every_sample"`,
            or `"lossless"`. Omit to default from the wire type's `flow_class`.

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
            "delivery_profile": delivery_profile,
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
