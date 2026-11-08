# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node.process` grammar — execution mode and ports, declared in code.

Nothing is read from disk: there is no manifest, and a bare `.py` module defines
a working node. `@node.process` validates the declaration and stamps it on the class as
`__tatolab_node_*__` attributes; it registers nothing. The runtime reads exactly
that set when a stream naming the class loads, so the set is the contract between
this module and the runtime's reader, and the two move together.
Ports are declared as `Input` and `Output` class attributes, one family each,
and accessed at run time through `ctx.inputs` / `ctx.outputs`.

A node is named by its class's import path, derived from `__module__` and
`__qualname__` by the runtime. Identity is never authored here.
"""

from __future__ import annotations

import dataclasses
import inspect
import sys
import typing
from collections.abc import Mapping
from typing import Any, Callable, NoReturn, Optional, ParamSpec, TypeVar, overload

from ._exposed_name_cast import (
    ExposedNameCastsToNothingError,
    cast_exposed_name_to_url_safe,
)
from ._function_node_declaration import FunctionNode, declare_function_node
from ._rust_function_node_declaration import RustFunctionNode, declare_rust_function_node
from ._port_family_declaration import Input, Output
from ._node_config_schema import (
    derive_config_class_json_schema,
    json_schema_for_a_node_declaring_no_config,
)


__all__ = [
    "AudioWindowContract",
    "node",
]

# The stamps `@node.process` writes and the runtime reads. The runtime's Rust reader
# spells them too, and its tests run this decorator against it.
NODE_DECLARATION_DECLARED_STAMP = "__tatolab_node_declared__"
NODE_DECLARATION_KIND_STAMP = "__tatolab_node_kind__"
NODE_DECLARATION_CONFIG_CLASS_STAMP = "__tatolab_node_config_class__"
NODE_DECLARATION_CONFIG_SCHEMA_STAMP = "__tatolab_node_config_schema__"
NODE_DECLARATION_DESCRIPTION_STAMP = "__tatolab_node_description__"
NODE_DECLARATION_EXECUTION_STAMP = "__tatolab_node_execution__"
NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP = "__tatolab_node_scheduling_priority__"
NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP = "__tatolab_node_paced_by_real_time__"
NODE_DECLARATION_INPUT_PORTS_STAMP = "__tatolab_node_input_ports__"
NODE_DECLARATION_OUTPUT_PORTS_STAMP = "__tatolab_node_output_ports__"

PROCESS_NODE_KIND = "process"

# Every kind stamps these, by the role each plays in the declaration.
_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES = {
    "declared": NODE_DECLARATION_DECLARED_STAMP,
    "kind": NODE_DECLARATION_KIND_STAMP,
    "config_class": NODE_DECLARATION_CONFIG_CLASS_STAMP,
    "config_schema": NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    "description": NODE_DECLARATION_DESCRIPTION_STAMP,
    "input_ports": NODE_DECLARATION_INPUT_PORTS_STAMP,
    "output_ports": NODE_DECLARATION_OUTPUT_PORTS_STAMP,
}

_FunctionNodeParameters = ParamSpec("_FunctionNodeParameters")
_FunctionNodeReturn = TypeVar("_FunctionNodeReturn")
"""The kind a class node is: it runs in a process of its own."""

_EXECUTION_MODES = ("reactive", "manual", "continuous")
_SCHEDULING_PRIORITIES = ("realtime", "high", "normal")
_AUDIO_WINDOW_DTYPES = ("f32", "f64")

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
stream — reachable here only so an `Input`'s `window=` can recognise and refuse it.

It is on no public surface: only a native node declares it, in
Rust, where a node opens the device stream that settles it. A Python node
never holds one.
"""


class AudioWindowContract:
    """The rate, dtype, window size and hop an audio input port wants, and the
    channel count only if it needs a particular one.

    Declared as a Channels input's `window=`, whose delivery must be
    `"ordered"`. Every argument is keyword-only. `window_size` counts
    per-channel samples — the unit `ChannelsBlock.samples_per_channel` uses —
    so one window carries `window_size * channels` samples. `hop` may be omitted and
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
                f"{', '.join(_AUDIO_WINDOW_DTYPES)}, the two a ChannelsBlock carries"
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
            f"AUDIO_WINDOW_MATCH_DEVICE, which no Python node can resolve. The "
            f"sentinel settles at setup() from the device stream the declaring node "
            f"opened, and a Python node runs in its own helper process — it opens no device "
            f"stream, and its window is its model's compile-time knowledge, not a "
            f"machine-varying device format. Declare an AudioWindowContract with the "
            f"values the model wants and the engine converts every block to them"
        )

    if delivery_profile == "newest":
        raise ValueError(
            f"input port {port_name!r} declares an audio_window, so it must declare "
            f"delivery_profile='ordered', not 'newest' — 'newest' skips to the latest message "
            f"by design, and an accumulator that needs contiguous samples would flush on "
            f"nearly every read"
        )

    if not isinstance(audio_window, AudioWindowContract):
        raise TypeError(
            f"input port {port_name!r} declares audio_window="
            f"{type(audio_window).__name__} — expected an AudioWindowContract"
        )

    return audio_window._as_declaration()


NodeClassUnderDeclaration = TypeVar("NodeClassUnderDeclaration", bound=type)


class NodeDeclarationDecorator:
    """The type of [`node`], whose attributes declare each node kind."""

    __slots__ = ()

    def __call__(self, *_arguments: object, **_keyword_arguments: object) -> NoReturn:
        """Refuse bare `@node`: every node names its kind."""
        raise TypeError(
            "bare `@node` declares no kind; every node names its kind. A class with "
            "`setup()` / `process()` and ports is `@node.process`, bare or with the same "
            "keyword arguments (`execution`, `interval_ms`, `scheduling`, "
            "`paced_by_real_time`, `description`). A function that computes its outputs "
            "from its inputs is `@node.function`."
        )

    @overload
    def function(
        self,
        node_function: "Callable[_FunctionNodeParameters, _FunctionNodeReturn]",
        *,
        description: str = "",
    ) -> "FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]": ...

    @overload
    def function(
        self, node_function: None = None, *, description: str = ""
    ) -> "Callable[[Callable[_FunctionNodeParameters, _FunctionNodeReturn]], FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]]": ...

    def function(
        self, node_function: "Optional[Callable[..., Any]]" = None, *, description: str = ""
    ) -> Any:
        """Mark a function as a function node: one that computes its outputs from its inputs.

        Usable bare (`@node.function`) or with `description=`. The signature is
        the node: each plain parameter is an input pin, its default the pin's
        value when nothing is wired, and `T | None = None` an optional input.
        The return annotation is the output `result`, or one output per field
        of a returned dataclass. `float`, `int` and `bool` are one-channel
        Channels, `dict` is Records, and a read type is its family. `config`
        names the node's one config dataclass. A parameter typed `Time` is
        handed the stream time of the read; one typed `State[Record]` the state
        the last call returned in its `Stateful[Output, Record]`. The decorated
        function is still called as itself.
        """

        def apply_to_function(
            function_under_decoration: "Callable[..., Any]",
        ) -> "FunctionNode[..., Any]":
            return declare_function_node(
                function_under_decoration,
                description=description,
                declaration_stamps=_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES,
            )

        if node_function is None:
            return apply_to_function
        return apply_to_function(node_function)

    def rust_function(self, native_declaration: "Mapping[str, Any]") -> "RustFunctionNode":
        """Declare a function node a module wrote in Rust with `#[function_node]`.

        Takes what its native half's `<function>_function_node_declaration()
        .to_python_declaration(py)` built, and is assigned at module scope to
        the node's short name, its import path: `Lerp = node.rust_function(_native.Lerp)`.
        """
        return declare_rust_function_node(
            native_declaration,
            module_name=sys._getframe(1).f_globals["__name__"],  # pyright: ignore[reportPrivateUsage]
            declaration_stamps=_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES,
        )

    def process(
        self,
        node_class: Optional[type] = None,
        *,
        execution: Optional[str] = None,
        interval_ms: int = 0,
        scheduling: Optional[str] = None,
        paced_by_real_time: bool = False,
        description: str = "",
    ) -> Any:
        """Mark a class as a process node: one that runs in a process of its own.

        Usable bare (`@node.process`) or with keyword arguments; its ports are `Input`
        and `Output` class attributes, each of one family. It declares execution,
        interval, scheduling priority and description — never identity: a node is
        named by the import path of the class it is, derived from `__module__` and
        `__qualname__`.

        Port names are cast to lowercase URL-safe characters, the way every exposed
        name is, so an attribute `Video` declares the port `video` and a lookup of
        either spelling finds it. Two ports casting alike are refused here.

        `execution` defaults to `"reactive"` for a class that declares at least one
        input port, and is required for one that declares none — a source has
        nothing to react to, so defaulting it there would produce a node that
        silently never runs.

        Configuration is one class, named by the annotation on the `config`
        parameter of `__init__` — a TypedDict, a dataclass or a model. A class
        whose `__init__` takes nothing beyond `self` declares no config and refuses
        one. Any other signature is refused here, at decoration. The class's JSON
        Schema is derived from its annotations and defaults and published in the
        node catalog, which is how an agent learns the keys before adding the
        node.

        Decorating registers nothing: it stamps the declaration on the class, and
        the class reaches the node catalog when a stream that uses it loads.
        `description` falls back to the class's docstring when it is not given.

        `paced_by_real_time=True` declares a node that paces itself on real time,
        as a live device does. It loads in a stream on any clock: what it writes
        is stamped with stream time, and in a stepped stream it stands outside
        the step barrier, which never waits for it.
        """

        def apply_to_class(class_under_decoration: NodeClassUnderDeclaration) -> NodeClassUnderDeclaration:
            return _declare_node(
                class_under_decoration,
                execution=execution,
                interval_ms=interval_ms,
                scheduling=scheduling,
                paced_by_real_time=paced_by_real_time,
                description=description,
            )

        if isinstance(node_class, type):
            return apply_to_class(node_class)

        if node_class is not None:
            raise TypeError(
                f"@node.process() takes no positional argument; got "
                f"{type(node_class).__name__}. A node is named by the import path "
                f"of the class it is — `my_app.filters:BlurEffect` — derived from "
                f"`__module__` and `__qualname__` and never authored. Use `@node.process` bare, "
                f"or with keyword arguments (`execution`, `interval_ms`, `scheduling`, "
                f"`paced_by_real_time`, "
                f"`description`)."
            )

        return apply_to_class

    def __repr__(self) -> str:
        return "node"


node = NodeDeclarationDecorator()
"""Declares a node by its kind: `@node.process` for a class whose `Input` and `Output` attributes are its ports, `@node.function` for a function whose signature is its ports."""


def _declare_node(
    node_class: NodeClassUnderDeclaration,
    *,
    execution: Optional[str],
    interval_ms: int,
    scheduling: Optional[str],
    paced_by_real_time: bool,
    description: str,
) -> NodeClassUnderDeclaration:
    input_ports, output_ports = _collect_declared_ports(node_class)
    config_class = _config_class_named_by_the_init_annotation(node_class)

    setattr(node_class, NODE_DECLARATION_DECLARED_STAMP, True)
    setattr(node_class, NODE_DECLARATION_KIND_STAMP, PROCESS_NODE_KIND)
    setattr(node_class, NODE_DECLARATION_CONFIG_CLASS_STAMP, config_class)
    setattr(
        node_class,
        NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
        json_schema_for_a_node_declaring_no_config()
        if config_class is None
        else derive_config_class_json_schema(config_class),
    )
    setattr(
        node_class,
        NODE_DECLARATION_DESCRIPTION_STAMP,
        description or inspect.getdoc(node_class) or "",
    )
    setattr(
        node_class,
        NODE_DECLARATION_EXECUTION_STAMP,
        _resolve_execution(
            execution, interval_ms, node_class, has_input_ports=bool(input_ports)
        ),
    )
    setattr(
        node_class,
        NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP,
        _validate_scheduling(scheduling),
    )
    setattr(
        node_class,
        NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP,
        _validate_paced_by_real_time(paced_by_real_time),
    )
    setattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, input_ports)
    setattr(node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, output_ports)

    return node_class


def _config_class_named_by_the_init_annotation(
    node_class: type,
) -> "Optional[type]":
    """The config class `node_class.__init__` names, or `None` for no config.

    Every other signature is refused here rather than when a graph holding the
    class loads: the decorator runs at import, which is the last moment an
    author is still looking at the class.
    """
    # A class defining no `__init__` inherits `object`'s, whose signature is
    # `(self, /, *args, **kwargs)` — a shape that would otherwise be refused.
    if node_class.__init__ is object.__init__:
        return None

    parameters = [
        parameter
        for name, parameter in inspect.signature(node_class.__init__).parameters.items()
        if name != "self"
    ]
    if not parameters:
        return None

    how_to_declare_a_config_class = (
        f"declare one parameter named `config`, annotated with the class its settings "
        f"live on — `def __init__(self, config: {node_class.__name__}Config) -> "
        f"None` — where that class is a TypedDict, a dataclass or a model. "
        f"`stream_builder.add(cls, config={{...}})` still passes a dict; the helper constructs the "
        f"class from it and hands the object in."
    )

    if len(parameters) > 1:
        raise TypeError(
            f"{node_class.__name__}.__init__ takes {len(parameters)} parameters "
            f"besides `self` ({', '.join(parameter.name for parameter in parameters)}); "
            f"a node's config is one class, not a parameter list. To fix: {how_to_declare_a_config_class}"
        )

    parameter = parameters[0]
    if parameter.kind is inspect.Parameter.VAR_KEYWORD:
        raise TypeError(
            f"{node_class.__name__}.__init__ takes `**{parameter.name}`; "
            f"keyword-argument configuration is not how a node is configured. "
            f"To fix: {how_to_declare_a_config_class}"
        )
    if parameter.kind is inspect.Parameter.VAR_POSITIONAL:
        raise TypeError(
            f"{node_class.__name__}.__init__ takes `*{parameter.name}`; a "
            f"node's config is one object, not a variadic. To fix: {how_to_declare_a_config_class}"
        )
    if parameter.name != "config":
        raise TypeError(
            f"{node_class.__name__}.__init__ takes `{parameter.name}`, but a "
            f"node's config parameter must be named `config`. To fix: {how_to_declare_a_config_class}"
        )
    if parameter.kind is inspect.Parameter.POSITIONAL_ONLY:
        raise TypeError(
            f"{node_class.__name__}.__init__ takes `config` positionally only, but "
            f"the helper constructs a node as `cls(config=...)`. To fix: drop the "
            f"`/` so `config` can be passed by name."
        )

    annotation = _resolved_init_annotations(node_class).get("config")
    if annotation is None:
        raise TypeError(
            f"{node_class.__name__}.__init__ takes `config` with no annotation, so "
            f"nothing names its config class and no schema can be derived. "
            f"To fix: {how_to_declare_a_config_class}"
        )
    if annotation is Any:
        raise TypeError(
            f"{node_class.__name__}.__init__ annotates `config` as `Any`, which "
            f"names no class, so the helper has nothing to construct and no schema can "
            f"be derived. To fix: {how_to_declare_a_config_class}"
        )
    if typing.get_origin(annotation) is not None or not isinstance(annotation, type):
        raise TypeError(
            f"{node_class.__name__}.__init__ annotates `config` as {annotation!r}, "
            f"which is not a class. A node's config is one class the helper "
            f"constructs. To fix: {how_to_declare_a_config_class}"
        )
    return annotation


def _resolved_init_annotations(node_class: type) -> "dict[str, Any]":
    try:
        return typing.get_type_hints(node_class.__init__, include_extras=True)
    except Exception as unresolvable:
        raise TypeError(
            f"{node_class.__name__}.__init__ carries an annotation that cannot be "
            f"resolved, so its config class cannot be read: {unresolvable}. The config "
            f"class must be importable at run time, not only under `TYPE_CHECKING`."
        ) from unresolvable


def _collect_declared_ports(
    node_class: type,
) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
    """Every `Input` / `Output` declaration, as the dicts the engine reads,
    each port under its cast name."""
    input_ports: "list[dict[str, Any]]" = []
    output_ports: "list[dict[str, Any]]" = []
    port_spelling_by_cast_name: "dict[str, str]" = {}
    for marker in _declared_port_markers(node_class):
        port_spelling = marker["name"]
        try:
            port_name = cast_exposed_name_to_url_safe(port_spelling)
        except ExposedNameCastsToNothingError as names_nothing:
            raise ValueError(
                f"{node_class.__name__} declares a port {port_spelling!r}: "
                f"{names_nothing}"
            ) from names_nothing
        first_spelling = port_spelling_by_cast_name.get(port_name)
        if first_spelling == port_spelling:
            raise ValueError(
                f"{node_class.__name__} declares the port name {port_name!r} more "
                f"than once — every port, input or output, needs its own name"
            )
        if first_spelling is not None:
            raise ValueError(
                f"{node_class.__name__} declares the ports {first_spelling!r} and "
                f"{port_spelling!r}, which both cast to {port_name!r} — a port name is "
                f"lowercased with its accents dropped and anything outside "
                f"a-z 0-9 - . _ ~ turned into '-', so two ports need names that stay "
                f"apart once cast"
            )
        port_spelling_by_cast_name[port_name] = port_spelling
        if "delivery_profile" in marker:
            declared_input = {
                "name": port_name,
                "description": marker["description"],
                "delivery_profile": marker["delivery_profile"],
            }
            if "borrow_quota" in marker:
                declared_input["borrow_quota"] = marker["borrow_quota"]
            if "audio_window" in marker:
                declared_input["audio_window"] = marker["audio_window"]
            if "family" in marker:
                declared_input["family"] = marker["family"]
            if "pin_default" in marker:
                declared_input["pin_default"] = marker["pin_default"]
            input_ports.append(declared_input)
        else:
            declared_output: "dict[str, Any]" = {
                "name": port_name,
                "description": marker["description"],
            }
            if "family" in marker:
                declared_output["family"] = marker["family"]
            output_ports.append(declared_output)
    return input_ports, output_ports


def _declared_port_markers(node_class: type) -> "list[dict[str, Any]]":
    """Every `Input` and `Output` class attribute declared on the class or
    inherited, in declaration order.

    Walks the MRO in reverse so a subclass's redeclaration of an inherited
    attribute wins, a base class's ports are inherited rather than lost, and a
    port a subclass shadows with anything else is no longer declared.
    """
    port_attributes_by_name: "dict[str, Input[Any] | Output[Any]]" = {}
    for ancestor in reversed(node_class.__mro__):
        for attribute_name, attribute in vars(ancestor).items():
            if isinstance(attribute, (Input, Output)):
                port_attributes_by_name[attribute_name] = attribute
            else:
                port_attributes_by_name.pop(attribute_name, None)
    return [attribute._as_port_marker() for attribute in port_attributes_by_name.values()]


def _resolve_execution(
    execution: Optional[str],
    interval_ms: int,
    node_class: type,
    *,
    has_input_ports: bool,
) -> "dict[str, Any]":
    if execution is None:
        if not has_input_ports:
            raise ValueError(
                f"{node_class.__name__} declares no input ports, so it must declare "
                f"an execution mode: `@node.process(execution=\"continuous\", interval_ms=…)` "
                f"for a source that produces on its own schedule, or "
                f"`execution=\"manual\"` for one driven by a callback it owns. Only a "
                f"node with an input port can default to \"reactive\"."
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


def _validate_paced_by_real_time(paced_by_real_time: bool) -> bool:
    if not isinstance(paced_by_real_time, bool):
        raise TypeError(
            f"invalid paced_by_real_time {paced_by_real_time!r}: must be True or False"
        )
    return paced_by_real_time


def _validate_scheduling(scheduling: Optional[str]) -> Optional[str]:
    if scheduling is None:
        return None
    if scheduling not in _SCHEDULING_PRIORITIES:
        raise ValueError(
            f"invalid scheduling {scheduling!r}: must be one of "
            f"{', '.join(_SCHEDULING_PRIORITIES)}"
        )
    return scheduling
