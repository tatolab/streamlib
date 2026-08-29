# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@processor` grammar — execution mode and ports, declared in code.

Nothing is read from disk: there is no manifest, and a bare `.py` module defines
a working processor. `@processor` attaches the metadata the engine reads at
`Runtime.add` time as `__streamlib_processor_*__` class attributes; that set is
the contract between this module and the native half, and the two move together.
Ports are declared with the `@input` / `@output` method decorators and accessed
at run time through `ctx.inputs` / `ctx.outputs` — the marker methods themselves
are never called.

A processor is named by its class's import path, derived from `__module__` and
`__qualname__` by the native half. Identity is never authored here.
"""

from __future__ import annotations

import dataclasses
from typing import Any, Callable, Optional, TypeVar, Union


__all__ = [
    "AUDIO_WINDOW_MATCH_DEVICE",
    "AudioWindowContract",
    "AudioWindowMatchDeviceSentinel",
    "input",
    "output",
    "processor",
]

_EXECUTION_MODES = ("reactive", "manual", "continuous")
_SCHEDULING_PRIORITIES = ("realtime", "high", "normal")
_DELIVERY_PROFILES = ("newest", "ordered")
_AUDIO_WINDOW_DTYPES = ("f32", "i16")


class AudioWindowMatchDeviceSentinel:
    """The type of [`AUDIO_WINDOW_MATCH_DEVICE`] — never constructed by an author."""

    __slots__ = ()

    def __repr__(self) -> str:
        return "AUDIO_WINDOW_MATCH_DEVICE"


AUDIO_WINDOW_MATCH_DEVICE = AudioWindowMatchDeviceSentinel()
"""Resolve the whole window contract at `setup()` from the device stream the
declaring processor opened, rather than stating five values here.

Whole-contract, never per-field, and only a processor that opens a device
stream can satisfy it.
"""


@dataclasses.dataclass(frozen=True)
class AudioWindowContract:
    """The rate, channels, dtype, window size and hop an audio input port wants.

    Declared beside `delivery_profile` on an `@input`, which must be
    `"ordered"`. `window_size` counts per-channel samples — the unit
    `AudioBlock.sample_count` uses — so one window carries
    `window_size * channels` scalars. An omitted `hop` resolves to
    `window_size` at construction, so the attribute always holds the real hop:
    contiguous, non-overlapping windows by default, a rolling window below
    that.

    All-or-nothing: there is no partial form, because a half-declared contract
    would leave the engine guessing at exactly the values a model asserts on.
    """

    sample_rate: int
    channels: int
    dtype: str
    window_size: int
    hop: Optional[int] = None

    def __post_init__(self) -> None:
        if self.hop is None:
            object.__setattr__(self, "hop", self.window_size)

        for field_name in ("sample_rate", "channels", "window_size", "hop"):
            value = getattr(self, field_name)
            if not isinstance(value, int) or isinstance(value, bool):
                raise TypeError(
                    f"AudioWindowContract field {field_name!r} must be an int; got "
                    f"{type(value).__name__}"
                )
            if value <= 0:
                raise ValueError(
                    f"AudioWindowContract field {field_name!r} is {value} — every numeric "
                    f"field is strictly positive. A zero hop makes no framing progress and "
                    f"a zero sample_rate resamples to nothing"
                )

        if self.dtype not in _AUDIO_WINDOW_DTYPES:
            raise ValueError(
                f"AudioWindowContract field 'dtype' is {self.dtype!r} — must be one of "
                f"{', '.join(_AUDIO_WINDOW_DTYPES)}, the two an AudioBlock legalises"
            )

        if self.hop is not None and self.hop > self.window_size:
            raise ValueError(
                f"AudioWindowContract declares hop {self.hop} above window_size "
                f"{self.window_size} — a hop above the window silently discards the samples "
                f"between windows. A hop below it is a rolling window and is legal; omitting "
                f"it makes windows contiguous"
            )

    def _as_declaration(self) -> "dict[str, Any]":
        """The wire shape the native half reads — identical to Rust's rendering."""
        return {
            "resolved_from": "declaration",
            "sample_rate": self.sample_rate,
            "channels": self.channels,
            "dtype": self.dtype,
            "window_size": self.window_size,
            "hop": self.hop,
        }


DeclaredAudioWindow = Union[AudioWindowContract, AudioWindowMatchDeviceSentinel]


def _audio_window_declaration(
    audio_window: DeclaredAudioWindow,
    port_name: str,
    delivery_profile: str,
) -> "dict[str, Any]":
    """Validate a declared window contract and render it for the native half."""
    if delivery_profile == "newest":
        raise ValueError(
            f"input port {port_name!r} declares an audio_window, so it must declare "
            f"delivery_profile='ordered', not 'newest' — 'newest' skips to the latest bag "
            f"by design, and an accumulator that needs contiguous samples would flush on "
            f"nearly every read"
        )

    if isinstance(audio_window, AudioWindowMatchDeviceSentinel):
        return {"resolved_from": "match_device"}

    if not isinstance(audio_window, AudioWindowContract):
        raise TypeError(
            f"input port {port_name!r} declares audio_window="
            f"{type(audio_window).__name__} — expected an AudioWindowContract or "
            f"AUDIO_WINDOW_MATCH_DEVICE"
        )

    return audio_window._as_declaration()

_INPUT_PORT_MARKER_ATTRIBUTE = "_streamlib_input_port"
_OUTPUT_PORT_MARKER_ATTRIBUTE = "_streamlib_output_port"

ProcessorClass = TypeVar("ProcessorClass", bound=type)
MethodUnderDecoration = TypeVar("MethodUnderDecoration", bound=Callable[..., Any])


def input(
    name: Optional[str] = None,
    *,
    description: str = "",
    delivery_profile: Optional[str] = None,
    audio_window: Optional[DeclaredAudioWindow] = None,
) -> "Callable[[MethodUnderDecoration], MethodUnderDecoration]":
    """Mark a method as declaring an input port.

    The port is named after the method unless `name` overrides it. The port
    carries no type — the method's return annotation is the declaration, read
    by humans and type checkers only. `delivery_profile` is required and names
    a read policy: `"newest"` drains to the most recent bag, `"ordered"`
    receives them in publication order. Neither promises delivery — both drop
    under sustained pressure, and no link ever blocks a producer. The
    decorated method is a declaration only: bags are read with
    `ctx.inputs.read(port_name)`.

    `audio_window` is optional and opt-in: an audio input may declare an
    [`AudioWindowContract`] or [`AUDIO_WINDOW_MATCH_DEVICE`], and the engine
    then resamples, converts channels and frames natively so `process()`
    receives exact-size blocks. A port declaring none is unchanged in every
    respect.
    """
    if delivery_profile is not None and delivery_profile not in _DELIVERY_PROFILES:
        raise ValueError(
            f"invalid delivery_profile {delivery_profile!r}: must be one of "
            f"{', '.join(_DELIVERY_PROFILES)}"
        )

    def attach_input_port_marker(method: MethodUnderDecoration) -> MethodUnderDecoration:
        port_name = name or method.__name__
        # `delivery_profile` defaults to None rather than being a required
        # keyword so the omission is caught here, where the port's name is
        # known — a bare TypeError from the call could not name it.
        if delivery_profile is None:
            raise ValueError(
                f"input port {port_name!r} must declare a delivery_profile — one of "
                f"{', '.join(_DELIVERY_PROFILES)}. There is no default: channel policy "
                f"is declared port-locally at the consuming input port"
            )
        marker: "dict[str, Any]" = {
            "name": port_name,
            "description": description,
            "delivery_profile": delivery_profile,
        }
        # Present only when declared: a contract-less port's marker is what it
        # always was, which is what makes the contract opt-in in the tree and
        # not only in the prose.
        if audio_window is not None:
            marker["audio_window"] = _audio_window_declaration(
                audio_window, port_name, delivery_profile
            )
        setattr(method, _INPUT_PORT_MARKER_ATTRIBUTE, marker)
        return method

    return attach_input_port_marker


def output(
    name: Optional[str] = None,
    *,
    description: str = "",
) -> "Callable[[MethodUnderDecoration], MethodUnderDecoration]":
    """Mark a method as declaring an output port.

    Same shape as [`input`], minus the delivery profile — delivery is the
    consuming port's policy. Bags are written with
    `ctx.outputs.write(port_name, bag)`.
    """

    def attach_output_port_marker(method: MethodUnderDecoration) -> MethodUnderDecoration:
        setattr(
            method,
            _OUTPUT_PORT_MARKER_ATTRIBUTE,
            {
                "name": name or method.__name__,
                "description": description,
            },
        )
        return method

    return attach_output_port_marker


def processor(
    processor_class: Optional[type] = None,
    *,
    execution: Optional[str] = None,
    interval_ms: int = 0,
    scheduling: Optional[str] = None,
    description: str = "",
) -> Any:
    """Mark a class as a streamlib processor.

    Usable bare (`@processor`) or with keyword arguments. It declares execution,
    interval, scheduling priority and description — never identity: a processor
    is named by the import path of the class it is, derived from `__module__`
    and `__qualname__`.

    `execution` defaults to `"reactive"` for a class that declares at least one
    input port, and is required for one that declares none — a source has
    nothing to react to, so defaulting it there would produce a processor that
    silently never runs.
    """
    if isinstance(processor_class, type):
        return _declare_processor(
            processor_class,
            execution=execution,
            interval_ms=interval_ms,
            scheduling=scheduling,
            description=description,
        )

    if processor_class is not None:
        raise TypeError(
            f"@processor() takes no positional argument; got "
            f"{type(processor_class).__name__}. A processor is named by the import path "
            f"of the class it is — `my_app.filters:BlurProcessor` — derived from "
            f"`__module__` and `__qualname__` and never authored. Use `@processor` bare, "
            f"or with keyword arguments (`execution`, `interval_ms`, `scheduling`, "
            f"`description`)."
        )

    def apply_to_class(class_under_decoration: ProcessorClass) -> ProcessorClass:
        return _declare_processor(
            class_under_decoration,
            execution=execution,
            interval_ms=interval_ms,
            scheduling=scheduling,
            description=description,
        )

    return apply_to_class


def _declare_processor(
    processor_class: ProcessorClass,
    *,
    execution: Optional[str],
    interval_ms: int,
    scheduling: Optional[str],
    description: str,
) -> ProcessorClass:
    input_ports, output_ports = _collect_declared_ports(processor_class)

    processor_class.__streamlib_processor_declared__ = True  # type: ignore[attr-defined]
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
    """Every `@input` / `@output` declaration, as the dicts the engine reads."""
    input_ports: "list[dict[str, Any]]" = []
    output_ports: "list[dict[str, Any]]" = []
    claimed_port_names: "set[str]" = set()
    for marker in _declared_port_markers(processor_class):
        port_name = marker["name"]
        if port_name in claimed_port_names:
            raise ValueError(
                f"{processor_class.__name__} declares the port name {port_name!r} more "
                f"than once — every port, input or output, needs its own name"
            )
        claimed_port_names.add(port_name)
        if "delivery_profile" in marker:
            declared_input = {
                "name": port_name,
                "description": marker["description"],
                "delivery_profile": marker["delivery_profile"],
            }
            if "audio_window" in marker:
                declared_input["audio_window"] = marker["audio_window"]
            input_ports.append(declared_input)
        else:
            output_ports.append(
                {
                    "name": port_name,
                    "description": marker["description"],
                }
            )
    return input_ports, output_ports


def _declared_port_markers(processor_class: type) -> "list[dict[str, Any]]":
    """Every port marker declared on the class or inherited, in declaration order.

    Walks the MRO in reverse so a subclass's redeclaration of an inherited
    method wins, and a base class's ports are inherited rather than lost.
    """
    markers_by_attribute: "dict[str, list[dict[str, Any]]]" = {}
    for ancestor in reversed(processor_class.__mro__):
        for attribute_name, attribute in vars(ancestor).items():
            attribute_markers = [
                marker
                for marker_attribute in (
                    _INPUT_PORT_MARKER_ATTRIBUTE,
                    _OUTPUT_PORT_MARKER_ATTRIBUTE,
                )
                for marker in [getattr(attribute, marker_attribute, None)]
                if marker is not None
            ]
            if attribute_markers:
                markers_by_attribute[attribute_name] = attribute_markers
    return [
        marker for markers in markers_by_attribute.values() for marker in markers
    ]


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
