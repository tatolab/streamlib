# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@processor` grammar — identity, execution mode, and ports, declared in code.

Nothing is read from disk: there is no manifest, and a bare `.py` module defines
a working processor. The decorator attaches the metadata the engine reads at
`Runtime.add` time as `__streamlib_processor_*__` class attributes; that set is
the contract between this module and the native half, and the two move together.
"""

from __future__ import annotations

import re
from typing import Any, Mapping, Optional, Pattern, TypeVar, Union

__all__ = [
    "LinkInputDataPort",
    "LinkOutputDataPort",
    "processor",
]

# Mirrors `streamlib_idents`' newtype validation. A processor reference is
# version-free — versions never appear at the code layer.
_ORGANIZATION_PATTERN: Pattern[str] = re.compile(r"^[a-z][a-z0-9-]*$")
_PACKAGE_PATTERN: Pattern[str] = re.compile(r"^[a-z][a-z0-9-]*$")
_TYPE_NAME_PATTERN: Pattern[str] = re.compile(r"^[A-Z][A-Za-z0-9]*$")
_IDENTITY_PATTERN: Pattern[str] = re.compile(r"^@([^/@]+)/([^/@]+)/([^/@]+)$")

_EXECUTION_MODES = ("reactive", "manual", "continuous")
_SCHEDULING_PRIORITIES = ("realtime", "high", "normal")
_DELIVERY_PROFILES = ("latest", "every_sample", "lossless")

ProcessorClass = TypeVar("ProcessorClass", bound=type)


class LinkInputDataPort:
    """An input port, declared as a class attribute and read from in `process()`.

    The attribute name is the port name, so a port is named once. Reading before
    the engine has bound the port raises rather than returning nothing.
    """

    def __init__(
        self,
        *,
        delivery_profile: Optional[str] = None,
        description: str = "",
    ) -> None:
        if delivery_profile is not None and delivery_profile not in _DELIVERY_PROFILES:
            raise ValueError(
                f"invalid delivery_profile {delivery_profile!r}: must be one of "
                f"{', '.join(_DELIVERY_PROFILES)}"
            )
        self.delivery_profile = delivery_profile
        self.description = description
        self.port_name: Optional[str] = None
        self._link_data_access: Optional[Any] = None

    def read(self) -> Optional[Any]:
        """The next bag on this port, or `None` when nothing is waiting.

        The bag arrives as ordinary Python data — a dict for the named map the
        wire carries. Consuming it is a cast at read time: the engine mediates
        no schema agreement, so what a producer declared is a hint, never a
        guarantee this call checks.
        """
        return self._bound_link_data_access().read_from_input_port(self._bound_name())

    def has_data(self) -> bool:
        """Whether a bag is waiting, without consuming it."""
        return self._bound_link_data_access().input_port_has_data(self._bound_name())

    def _bound_name(self) -> str:
        name = self.port_name
        if name is None:
            raise RuntimeError(
                "this input port was never named — declare it as a class attribute of "
                "a @processor class, where the attribute name becomes the port name"
            )
        return name

    def _bound_link_data_access(self) -> Any:
        if self._link_data_access is None:
            raise RuntimeError(
                f"input port {self.port_name!r} is not bound to a running processor — "
                f"ports are bound by the engine when the processor is constructed, so "
                f"reading from a class attribute or a hand-instantiated processor "
                f"cannot work. Use streamlib.testing to drive a processor in a test."
            )
        return self._link_data_access

    def __repr__(self) -> str:
        return f"LinkInputDataPort(name={self.port_name!r}, bound={self._link_data_access is not None})"


class LinkOutputDataPort:
    """An output port, declared as a class attribute and written to in `process()`."""

    def __init__(self, *, description: str = "") -> None:
        self.description = description
        self.port_name: Optional[str] = None
        self._link_data_access: Optional[Any] = None

    def write(self, bag: "Mapping[str, Any]") -> None:
        """Publish one bag to every downstream link on this port.

        A bag is a named map: a dict with string keys, whose values are ordinary
        Python data — dicts, lists, tuples, str, bytes, int, float, bool, None.
        The wire carries a named map because a processor in another language
        reads it into a struct, so a list or a non-string key is refused rather
        than published as bytes only Python can decode.
        """
        self._bound_link_data_access().write_to_output_port(self._bound_name(), bag)

    def _bound_name(self) -> str:
        name = self.port_name
        if name is None:
            raise RuntimeError(
                "this output port was never named — declare it as a class attribute of "
                "a @processor class, where the attribute name becomes the port name"
            )
        return name

    def _bound_link_data_access(self) -> Any:
        if self._link_data_access is None:
            raise RuntimeError(
                f"output port {self.port_name!r} is not bound to a running processor — "
                f"ports are bound by the engine when the processor is constructed, so "
                f"writing from a class attribute or a hand-instantiated processor "
                f"cannot work. Use streamlib.testing to drive a processor in a test."
            )
        return self._link_data_access

    def __repr__(self) -> str:
        return f"LinkOutputDataPort(name={self.port_name!r}, bound={self._link_data_access is not None})"


AnyDataPort = Union[LinkInputDataPort, LinkOutputDataPort]


def processor(
    class_or_identity: Union[type, str, None] = None,
    *,
    execution: Optional[str] = None,
    interval_ms: int = 0,
    scheduling: Optional[str] = None,
    description: str = "",
) -> Any:
    """Mark a class as a streamlib processor.

    Usable bare (`@processor`) or with arguments. Omitting the identity
    synthesizes `@app/local/<ClassName>`, so an app-local processor needs no
    identity at all; pass a version-free `@org/package/Type` string to publish
    one under a shared name.

    `execution` defaults to `"reactive"` for a class that declares at least one
    input port, and is required for one that declares none — a source has
    nothing to react to, so defaulting it there would produce a processor that
    silently never runs.
    """
    if isinstance(class_or_identity, type):
        return _declare_processor(
            class_or_identity,
            identity=None,
            execution=execution,
            interval_ms=interval_ms,
            scheduling=scheduling,
            description=description,
        )

    if class_or_identity is not None and not isinstance(class_or_identity, str):
        raise TypeError(
            f"@processor() takes a version-free `@org/package/Type` identity string or "
            f"nothing at all; got {type(class_or_identity).__name__}."
        )

    identity = class_or_identity

    def apply_to_class(processor_class: ProcessorClass) -> ProcessorClass:
        return _declare_processor(
            processor_class,
            identity=identity,
            execution=execution,
            interval_ms=interval_ms,
            scheduling=scheduling,
            description=description,
        )

    return apply_to_class


def _declare_processor(
    processor_class: ProcessorClass,
    *,
    identity: Optional[str],
    execution: Optional[str],
    interval_ms: int,
    scheduling: Optional[str],
    description: str,
) -> ProcessorClass:
    input_ports, output_ports = _collect_declared_ports(processor_class)

    processor_class.__streamlib_processor_type_reference__ = _resolve_type_reference(  # type: ignore[attr-defined]
        identity, processor_class
    )
    processor_class.__streamlib_processor_description__ = description  # type: ignore[attr-defined]
    processor_class.__streamlib_processor_execution__ = _resolve_execution(  # type: ignore[attr-defined]
        execution, interval_ms, processor_class, has_input_ports=bool(input_ports)
    )
    processor_class.__streamlib_processor_scheduling_priority__ = _validate_scheduling(  # type: ignore[attr-defined]
        scheduling
    )
    processor_class.__streamlib_processor_input_ports__ = input_ports  # type: ignore[attr-defined]
    processor_class.__streamlib_processor_output_ports__ = output_ports  # type: ignore[attr-defined]
    return processor_class


def _collect_declared_ports(
    processor_class: type,
) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
    """Name every declared port after the attribute holding it."""
    input_ports: "list[dict[str, Any]]" = []
    output_ports: "list[dict[str, Any]]" = []
    for attribute_name, declaration in _declared_ports(processor_class):
        declaration.port_name = attribute_name
        if isinstance(declaration, LinkInputDataPort):
            input_ports.append(
                {
                    "name": attribute_name,
                    "description": declaration.description,
                    "delivery_profile": declaration.delivery_profile,
                }
            )
        else:
            output_ports.append(
                {"name": attribute_name, "description": declaration.description}
            )
    return input_ports, output_ports


def _resolve_type_reference(
    identity: Optional[str], processor_class: type
) -> "dict[str, str]":
    if identity is None:
        type_name = processor_class.__name__
        if not _TYPE_NAME_PATTERN.match(type_name):
            raise ValueError(
                f"cannot synthesize an `@app/local` identity for class {type_name!r}: a "
                f"processor type name must be PascalCase (`^[A-Z][A-Za-z0-9]*$`). Give "
                f"the class a PascalCase name, or declare an explicit "
                f"`@org/package/Type` identity."
            )
        return {"org": "app", "package": "local", "type": type_name}

    if "@" in identity[1:]:
        raise ValueError(
            f"processor identity {identity!r} must be version-free "
            f"`@<org>/<package>/<Type>` with no `@<version>` — versions never appear at "
            f"the code layer."
        )
    match = _IDENTITY_PATTERN.match(identity)
    if match is None:
        raise ValueError(
            f"processor identity {identity!r} must be `@<org>/<package>/<Type>` "
            f"(exactly three `/`-separated segments, leading `@`)"
        )
    organization, package, type_name = match.groups()
    for value, pattern, label in (
        (organization, _ORGANIZATION_PATTERN, "org"),
        (package, _PACKAGE_PATTERN, "package"),
        (type_name, _TYPE_NAME_PATTERN, "type"),
    ):
        if not pattern.match(value):
            raise ValueError(
                f"processor identity {identity!r} has an invalid {label} segment "
                f"{value!r}: must match {pattern.pattern}"
            )
    return {"org": organization, "package": package, "type": type_name}


def _resolve_execution(
    execution: Optional[str],
    interval_ms: int,
    processor_class: type,
    *,
    has_input_ports: bool,
) -> "dict[str, Any]":
    if execution is None:
        if not has_input_ports:
            raise ValueError(
                f"{processor_class.__name__} declares no input ports, so it must declare "
                f"an execution mode: `@processor(execution=\"continuous\", interval_ms=…)` "
                f"for a source that produces on its own schedule, or "
                f"`execution=\"manual\"` for one driven by a callback it owns. Only a "
                f"processor with an input port can default to \"reactive\"."
            )
        execution = "reactive"

    if execution not in _EXECUTION_MODES:
        raise ValueError(
            f"invalid execution {execution!r}: must be one of "
            f"{', '.join(_EXECUTION_MODES)}"
        )
    if execution != "continuous":
        return {"mode": execution, "interval_ms": 0}

    if not isinstance(interval_ms, int) or isinstance(interval_ms, bool) or interval_ms < 0:
        raise ValueError(
            f"invalid interval_ms {interval_ms!r}: must be a non-negative int"
        )
    return {"mode": "continuous", "interval_ms": interval_ms}


def _validate_scheduling(scheduling: Optional[str]) -> Optional[str]:
    if scheduling is None:
        return None
    if scheduling not in _SCHEDULING_PRIORITIES:
        raise ValueError(
            f"invalid scheduling {scheduling!r}: must be one of "
            f"{', '.join(_SCHEDULING_PRIORITIES)}"
        )
    return scheduling


def bind_declared_ports_to_running_processor(
    processor_instance: Any, link_data_access: Any
) -> None:
    """Give a freshly constructed processor its own bound copy of every port.

    Called by the engine, never by app code. The declarations live on the class
    and are therefore shared by every instance of it, so binding sets a fresh
    per-instance port on the object rather than mutating what the class holds —
    two instances of one processor class read different links.
    """
    for attribute_name, declaration in _declared_ports(type(processor_instance)):
        bound: AnyDataPort
        if isinstance(declaration, LinkInputDataPort):
            bound = LinkInputDataPort(
                delivery_profile=declaration.delivery_profile,
                description=declaration.description,
            )
        else:
            bound = LinkOutputDataPort(description=declaration.description)
        bound.port_name = attribute_name
        bound._link_data_access = link_data_access
        setattr(processor_instance, attribute_name, bound)


def _declared_ports(processor_class: type) -> "list[tuple[str, AnyDataPort]]":
    """Every port declared on the class or inherited, in declaration order.

    Walks the MRO in reverse so a subclass's redeclaration of an inherited port
    name wins, and a base class's ports are inherited rather than lost.
    """
    declarations: "dict[str, AnyDataPort]" = {}
    for ancestor in reversed(processor_class.__mro__):
        for attribute_name, attribute in vars(ancestor).items():
            if isinstance(attribute, (LinkInputDataPort, LinkOutputDataPort)):
                declarations[attribute_name] = attribute
    return list(declarations.items())
