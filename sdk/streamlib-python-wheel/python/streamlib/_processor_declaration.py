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
from typing import Any, Callable, Optional, TypeVar


__all__ = [
    "AudioWindowContract",
    "input",
    "output",
    "processor",
]

_EXECUTION_MODES = ("reactive", "manual", "continuous")
_SCHEDULING_PRIORITIES = ("realtime", "high", "normal")
_DELIVERY_PROFILES = ("newest", "ordered")
_AUDIO_WINDOW_DTYPES = ("f32", "i16")

AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE = "source"
"""How an omitted channel count is spelled on the wire the native half reads.

The same word Rust renders, so `graph` shows one spelling whichever language
declared the port."""


class AudioWindowMatchDeviceSentinel:
    """The type of [`AUDIO_WINDOW_MATCH_DEVICE`] — never constructed by an author."""

    __slots__ = ()

    def __repr__(self) -> str:
        return "AUDIO_WINDOW_MATCH_DEVICE"


AUDIO_WINDOW_MATCH_DEVICE = AudioWindowMatchDeviceSentinel()
"""The engine-side spelling for a contract resolved at `setup()` from a device
stream — reachable here only so [`input`] can recognise and refuse it.

It is on no public surface: a native built-in like `SpeakerSink` declares it in
Rust, where a processor opens the device stream that settles it. A Python
processor never holds one.
"""


class AudioWindowContract:
    """The rate, dtype, window size and hop an audio input port wants, and the
    channel count only if it needs a particular one.

    Declared beside `delivery_profile` on an `@input`, which must be
    `"ordered"`. Every argument is keyword-only. `window_size` counts
    per-channel samples — the unit `AudioBlock.sample_count` uses — so one
    window carries `window_size * channels` scalars. `hop` may be omitted and
    then resolves to `window_size`: contiguous, non-overlapping windows by
    default, a rolling window below that. The attribute always holds the
    resolved hop, never `None`, which is why the constructor takes the
    omittable spelling and the attribute does not.

    `channels` may be omitted too, and then means *the source's own count,
    whatever it is*: the engine converts nothing and every window carries the
    count its block arrived with, so read `channels` off each block rather than
    assuming it. That is the default because a graph is dynamic — a microphone
    added later must not require editing every consumer downstream of it. State
    a count only where something asserts on it, such as a model trained on
    mono, and the engine converts to it by the fixed rule. Unlike `hop`, the
    attribute stays `None` when it was omitted: there is no count to resolve it
    to until a block arrives.

    All-or-nothing otherwise: the remaining values have no partial form,
    because a half-declared contract would leave the engine guessing at exactly
    the values a model asserts on.
    """

    __slots__ = ("sample_rate", "channels", "dtype", "window_size", "hop")

    sample_rate: int
    channels: Optional[int]
    dtype: str
    window_size: int
    hop: int

    def __init__(
        self,
        *,
        sample_rate: int,
        dtype: str,
        window_size: int,
        hop: Optional[int] = None,
        channels: Optional[int] = None,
    ) -> None:
        resolved_hop = window_size if hop is None else hop
        numeric_fields = [
            ("sample_rate", sample_rate),
            ("window_size", window_size),
            ("hop", resolved_hop),
        ]
        # There is nothing to check about a count nobody wrote: absent means the
        # source's own, which arrives with the blocks rather than here.
        if channels is not None:
            numeric_fields.append(("channels", channels))
        for field_name, value in numeric_fields:
            if not isinstance(value, int) or isinstance(value, bool):
                raise TypeError(
                    f"AudioWindowContract field {field_name!r} must be an int; got "
                    f"{type(value).__name__}"
                )
            if value <= 0:
                raise ValueError(
                    f"AudioWindowContract field {field_name!r} is {value} — every numeric "
                    f"field is strictly positive"
                )

        if dtype not in _AUDIO_WINDOW_DTYPES:
            raise ValueError(
                f"AudioWindowContract field 'dtype' is {dtype!r} — must be one of "
                f"{', '.join(_AUDIO_WINDOW_DTYPES)}, the two an AudioBlock legalises"
            )

        if resolved_hop > window_size:
            raise ValueError(
                f"AudioWindowContract declares hop {resolved_hop} above window_size "
                f"{window_size} — a hop above the window silently discards the samples "
                f"between windows. A hop below it is a rolling window and is legal; omitting "
                f"it makes windows contiguous"
            )

        for field_name, value in (
            ("sample_rate", sample_rate),
            ("channels", channels),
            ("dtype", dtype),
            ("window_size", window_size),
            ("hop", resolved_hop),
        ):
            object.__setattr__(self, field_name, value)

    def __setattr__(self, name: str, value: Any) -> None:
        raise dataclasses.FrozenInstanceError(
            f"AudioWindowContract is frozen; cannot assign to {name!r}"
        )

    def __delattr__(self, name: str) -> None:
        raise dataclasses.FrozenInstanceError(
            f"AudioWindowContract is frozen; cannot delete {name!r}"
        )

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, AudioWindowContract):
            return NotImplemented
        return self._as_declaration() == other._as_declaration()

    def __hash__(self) -> int:
        return hash(
            (self.sample_rate, self.channels, self.dtype, self.window_size, self.hop)
        )

    def __repr__(self) -> str:
        return (
            f"AudioWindowContract(sample_rate={self.sample_rate}, "
            f"channels={self.channels}, dtype={self.dtype!r}, "
            f"window_size={self.window_size}, hop={self.hop})"
        )

    def _as_declaration(self) -> "dict[str, Any]":
        """The wire shape the native half reads — identical to Rust's rendering.

        An omitted count is spelled rather than left out, so a reader learns it
        follows the source where a missing key would tell it nothing.
        """
        return {
            "resolved_from": "declaration",
            "sample_rate": self.sample_rate,
            "channels": (
                AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE
                if self.channels is None
                else self.channels
            ),
            "dtype": self.dtype,
            "window_size": self.window_size,
            "hop": self.hop,
        }


def _audio_window_declaration(
    audio_window: object,
    port_name: str,
    delivery_profile: str,
) -> "dict[str, Any]":
    """Validate a declared window contract and render it for the native half."""
    if isinstance(audio_window, AudioWindowMatchDeviceSentinel):
        raise TypeError(
            f"input port {port_name!r} declares audio_window="
            f"AUDIO_WINDOW_MATCH_DEVICE, which no Python processor can resolve. The "
            f"sentinel settles at setup() from the device stream the declaring processor "
            f"opened, and every Python processor is helper-placed — it opens no device "
            f"stream, and its window is its model's compile-time knowledge, not a "
            f"machine-varying device format. Declare an AudioWindowContract with the five "
            f"values the model wants and the engine converts every block to them"
        )

    if delivery_profile == "newest":
        raise ValueError(
            f"input port {port_name!r} declares an audio_window, so it must declare "
            f"delivery_profile='ordered', not 'newest' — 'newest' skips to the latest bag "
            f"by design, and an accumulator that needs contiguous samples would flush on "
            f"nearly every read"
        )

    if not isinstance(audio_window, AudioWindowContract):
        raise TypeError(
            f"input port {port_name!r} declares audio_window="
            f"{type(audio_window).__name__} — expected an AudioWindowContract"
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
    audio_window: Optional[AudioWindowContract] = None,
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
    [`AudioWindowContract`], stating the rate, dtype, window size and hop it
    wants, and a channel count only where it needs a particular one — absent,
    every window carries the source's own count. A port declaring no contract
    at all is unchanged in every respect.
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
