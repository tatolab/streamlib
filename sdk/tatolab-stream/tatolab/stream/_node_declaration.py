# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@node` grammar of every node kind.

Nothing is read from disk: there is no manifest, and a bare `.py` module defines
a working node. `@node` validates the declaration and stamps it on the class or function as
`__tatolab_node_*__` attributes; it registers nothing. The runtime reads exactly
that set when a stream naming the node loads, so the set is the contract between
this module and the runtime's reader, and the two move together.

A node is named by its import path, derived from `__module__` and
`__qualname__` by the runtime. Identity is never authored here.
"""

from __future__ import annotations

import dataclasses
import inspect
import sys
from collections.abc import Mapping
from typing import Any, Callable, Literal, NoReturn, Optional, ParamSpec, Protocol, TypeVar, overload

from ._class_node_declaration import (
    read_class_node_ports_members_and_pin_groups,
    read_node_function_ports_and_members,
)
from ._function_node_declaration import FunctionNode, declare_function_node
from ._node_meta_declaration import (
    PROCESS_NODE_KIND,
    hold_node_meta_for_the_kind_decorator,
    read_node_meta,
    take_node_meta_held_for_the_kind_decorator,
    the_conversion_a_node_states,
)
from ._node_function import NodeFunction
from ._rust_function_node_declaration import RustFunctionNode, declare_rust_function_node
from ._shader_node_declaration import ShaderNode, declare_shader_node
from ._pin_declaration import NodeDeclarationRefusal
from ._retired_declarations import (
    RETIRED_DECLARATION_FINDING_CODE,
    RETIRED_DECLARATIONS,
    RetiredDeclaration,
    the_words_refusing_a_retired_declaration,
)


__all__ = [
    "AudioWindowContract",
    "node",
]

# The stamps `@node` writes and the runtime reads. The runtime's Rust reader
# spells them too, and its tests run this decorator against it.
NODE_DECLARATION_DECLARED_STAMP = "__tatolab_node_declared__"
NODE_DECLARATION_KIND_STAMP = "__tatolab_node_kind__"
NODE_DECLARATION_DESCRIPTION_STAMP = "__tatolab_node_description__"
NODE_DECLARATION_CATEGORY_STAMP = "__tatolab_node_category__"
NODE_DECLARATION_CONVERSION_STAMP = "__tatolab_node_conversion__"
# The decorator that read the node's ports, which `@node.meta` above it reads; never described.
NODE_DECLARATION_DECLARED_BY_STAMP = "__tatolab_node_declared_by__"
NODE_DECLARATION_EXECUTION_STAMP = "__tatolab_node_execution__"
NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP = "__tatolab_node_scheduling_priority__"
NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP = "__tatolab_node_paced_by_real_time__"
NODE_DECLARATION_INPUT_PORTS_STAMP = "__tatolab_node_input_ports__"
NODE_DECLARATION_OUTPUT_PORTS_STAMP = "__tatolab_node_output_ports__"
# The field or pin behind each port of a class under bare `@node`, which its helper builds it from.
NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP = "__tatolab_class_node_members__"
# The pin groups of a class under bare `@node`: as the runtime reads them, and as its helper
# builds each slot.
NODE_DECLARATION_PIN_GROUPS_STAMP = "__tatolab_node_pin_groups__"
NODE_DECLARATION_CLASS_NODE_PIN_GROUPS_STAMP = "__tatolab_class_node_pin_groups__"

# Every kind stamps these, by the role each plays in the declaration.
_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES = {
    "declared": NODE_DECLARATION_DECLARED_STAMP,
    "kind": NODE_DECLARATION_KIND_STAMP,
    "description": NODE_DECLARATION_DESCRIPTION_STAMP,
    "category": NODE_DECLARATION_CATEGORY_STAMP,
    "conversion": NODE_DECLARATION_CONVERSION_STAMP,
    "declared_by": NODE_DECLARATION_DECLARED_BY_STAMP,
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


class AudioWindowContract:
    """The rate, dtype, window size and hop an audio input port wants, and the
    channel count only if it needs a particular one.

    Written beside an `Audio` pin's type, `InputPin[Annotated[Audio, StreamDelivery("ordered"),
    AudioWindowContract(...)]]`, whose delivery must be `"ordered"`. Every argument is keyword-only. `window_size` counts
    per-channel samples — the unit `Audio.samples_per_channel` uses —
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
                f"{', '.join(_AUDIO_WINDOW_DTYPES)}, the two an Audio block carries"
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
    delivery_profile: Optional[str],
) -> "dict[str, Any]":
    """Validate a declared window contract and render it for the native half."""
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

# What a class node's keyword arguments are when none is written.
_CLASS_NODE_KEYWORD_DEFAULTS: "dict[str, Any]" = {
    "execution": None,
    "interval_ms": 0,
    "scheduling": None,
    "paced_by_real_time": False,
}
_NodeUnderDeclaration = TypeVar("_NodeUnderDeclaration")
_ProcessNodeUnderDeclaration = TypeVar("_ProcessNodeUnderDeclaration")


class _NodeDeclarationItsKeywordsSay(Protocol):
    """What `@node(...)` with its keywords is: it declares a class node, or a node function."""

    @overload
    def __call__(self, node: NodeClassUnderDeclaration, /) -> NodeClassUnderDeclaration: ...

    @overload
    def __call__(
        self, node: "Callable[_FunctionNodeParameters, _FunctionNodeReturn]", /
    ) -> NodeFunction: ...


class NodeDeclarationDecorator:
    """The type of [`node`], whose attributes declare each node kind."""

    __slots__ = ()

    @overload
    def __call__(self, node: NodeClassUnderDeclaration, /) -> NodeClassUnderDeclaration: ...

    @overload
    def __call__(
        self, node: "Callable[_FunctionNodeParameters, _FunctionNodeReturn]", /
    ) -> "FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]": ...

    @overload
    def __call__(
        self,
        node: None = None,
        /,
        *,
        execution: Optional[str] = None,
        interval_ms: int = 0,
        scheduling: Optional[str] = None,
        paced_by_real_time: bool = False,
    ) -> "_NodeDeclarationItsKeywordsSay": ...

    def __call__(
        self,
        node: "Optional[type | Callable[..., Any]]" = None,
        /,
        *,
        execution: Optional[str] = None,
        interval_ms: int = 0,
        scheduling: Optional[str] = None,
        paced_by_real_time: bool = False,
    ) -> Any:
        """Declare a function or a class as a node whose pins each state their pin type.

        On a function, each plain parameter is an input, `Annotated[float,
        NumberRange(minimum=0.0, maximum=1.0), NumberUnit("Hz"), PinMeaning("frequency")] =
        440.0`: its type is the pin type, the objects beside it are what the pin states
        besides, and its default is the value the input holds until one is set or linked.
        `T | None = None` is an input that may be unset and reads `None`. A renamed pin writes
        the names it had beside it, `FormerlyNamed("frequency_hz")`, and a project saved under
        one of them opens with its value and links on the pin. The return annotation is the
        output `result`. `Time`, `State[...]` and `Stateful[...]` are found by type and
        are never pins. Bare `@node` on a function declares a pure function node.

        `@node(...)` on a function declares a node written as one function, whose body runs once
        when the node is set up and returns the function it calls each frame. Each parameter is
        keyword-only: a value input `level: Input[float] = Input(0.25)`, a pin `video:
        InputPin[Texture]` or `result: OutputPin[Texture]`, or the context `ctx:
        RuntimeContextFullAccess`. It takes the keywords a class takes.

        On a class, each value input is an annotated field with its default, `radius: float =
        4.0`, each Stream or Trigger input `video: InputPin[Texture]`, and each output
        `result: OutputPin[Texture]`. `SettingOnly()` beside a field's type makes it a setting
        read at setup, `WireOnly()` one that takes only a wire, and `InputValueLifetime(...)`
        states how long its value lasts; `StreamDelivery(...)` inside an `InputPin` states how
        it reads its messages. `__init__` takes no parameters, and state the node keeps is
        `_private` or a `ClassVar`. A class takes `@node` bare or with `execution`,
        `interval_ms`, `scheduling` and `paced_by_real_time`.

        What describes the node is `@node.meta`. A mistake is refused at decoration as a
        `NodeDeclarationRefusal`, and a pin type the runtime does not carry yet is refused by
        name.
        """
        class_keywords = {
            "execution": execution,
            "interval_ms": interval_ms,
            "scheduling": scheduling,
            "paced_by_real_time": paced_by_real_time,
        }

        def apply_to_class_or_function(node_under_decoration: Any) -> Any:
            if not isinstance(node_under_decoration, type):
                return _declare_a_node_function(node_under_decoration, **class_keywords)
            input_ports, output_ports, members, pin_groups = (
                read_class_node_ports_members_and_pin_groups(node_under_decoration)
            )
            setattr(node_under_decoration, NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP, members)
            setattr(node_under_decoration, NODE_DECLARATION_CLASS_NODE_PIN_GROUPS_STAMP, pin_groups)
            return _stamp_a_process_node(
                node_under_decoration,
                input_ports=input_ports,
                output_ports=output_ports,
                pin_groups=[pin_group.declared for pin_group in pin_groups],
                **class_keywords,  # pyright: ignore[reportArgumentType]
            )

        if node is None:
            return apply_to_class_or_function
        if isinstance(node, type):
            return apply_to_class_or_function(node)
        if class_keywords != _CLASS_NODE_KEYWORD_DEFAULTS:
            raise TypeError(
                "`execution`, `interval_ms`, `scheduling` and `paced_by_real_time` declare how a "
                "node written as one function runs, and a function under bare `@node` is a pure "
                "function node, which runs inside the read of each node it feeds. To fix: "
                "`@node(execution=...)` with its keywords inside the call."
            )
        return declare_function_node(
            node,
            declaration_stamps=_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES,
        )

    def meta(
        self,
        *,
        category: Optional[str] = None,
        description: Optional[str] = None,
        conversion: Optional[Literal[True]] = None,
    ) -> "Callable[[_NodeUnderDeclaration], _NodeUnderDeclaration]":
        """Declare what describes a node type: its `category`, its `description`, and whether it is a conversion.

        Written above or below the decorator that says the node's kind, and the same
        on every kind. `category` is the group an editor files the node type under,
        `"Effects"`, or a path into a group inside one, `"Effects/Stylize"`; a node
        type that declares none has none. `description` is CommonMark with no raw
        HTML, served exactly as written, and a docstring never stands in for it.
        Each is a string written in the code, which is what lets a project's node
        types be listed from source without running them.

        `conversion=True` states that the node converts the pin type of its one input that
        takes a wire into that of its one output; every other input is `SettingOnly()`, or a
        value input of a node written as one function. Its
        pins say what it bridges. A link only it bridges is refused naming it, with the call
        that places it, and `connect` never places it itself. Only bare `@node` and
        `@node.shader` declare one, and any other shape is refused at decoration as a
        `NodeDeclarationRefusal`. A node that is no conversion leaves it out.
        """
        declared = read_node_meta(
            category=category, description=description, conversion=conversion
        )

        def apply_to_node(node_under_declaration: _NodeUnderDeclaration) -> _NodeUnderDeclaration:
            if vars(node_under_declaration).get(NODE_DECLARATION_DECLARED_STAMP) is True:
                stamped: "dict[str, Any]" = dict(declared)
                if "conversion" in declared:
                    stamped["conversion"] = the_conversion_a_node_states(
                        getattr(node_under_declaration, "__name__"),
                        declared,
                        declared_by=getattr(node_under_declaration, NODE_DECLARATION_DECLARED_BY_STAMP),
                        kind=getattr(node_under_declaration, NODE_DECLARATION_KIND_STAMP),
                        input_ports=getattr(node_under_declaration, NODE_DECLARATION_INPUT_PORTS_STAMP),
                        output_ports=getattr(node_under_declaration, NODE_DECLARATION_OUTPUT_PORTS_STAMP),
                        pin_groups=getattr(node_under_declaration, NODE_DECLARATION_PIN_GROUPS_STAMP, []),
                        a_node_function=isinstance(node_under_declaration, NodeFunction),
                    )
                for meta_key, stamped_value in stamped.items():
                    setattr(
                        node_under_declaration,
                        _DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES[meta_key],
                        stamped_value,
                    )
            else:
                hold_node_meta_for_the_kind_decorator(node_under_declaration, declared)
            return node_under_declaration

        return apply_to_node

    def function(self, *_arguments: object, **_keyword_arguments: object) -> NoReturn:
        """Refuse the retired `@node.function` with what replaces it: bare `@node`."""
        _refuse_the_retired(_RETIRED_FUNCTION_NODE_FORM, _arguments)

    def shader(
        self, shader_source: str
    ) -> "Callable[[Callable[_FunctionNodeParameters, _FunctionNodeReturn]], ShaderNode[_FunctionNodeParameters, _FunctionNodeReturn]]":
        """Mark a function as a shader node: a Slang function from Images and values to an Image.

        `shader_source` is the node's body, a Slang function named for the node
        returning the `float4` pixel it computes; the decorated function's
        signature declares the ports and its body is `...`. Each `Texture`
        parameter arrives in Slang as an `ImageInput`, whose `pixel` is the
        input at this pixel, whose `uv` is this pixel's position from 0 to 1,
        and whose `sample(uv)` reads it at any position, bilinear; each `float`
        arrives as a value, a push constant. The return
        is one `Texture`, the output `result`, at the extent of the first
        `Texture` input. The runtime compiles and runs it on the GPU with no
        Python process, and takes RGBA Images only.
        """

        def apply_to_function(
            function_under_decoration: "Callable[..., Any]",
        ) -> "ShaderNode[..., Any]":
            return declare_shader_node(
                function_under_decoration,
                shader_source=shader_source,
                declaration_stamps=_DECLARATION_STAMPS_EVERY_NODE_KIND_CARRIES,
            )

        if not isinstance(shader_source, str):
            raise TypeError(
                "@node.shader takes the node's Slang source as its one argument, "
                '`@node.shader("""float4 tint(ImageInput image, float amount) { ... }""")`, '
                f"got {shader_source!r}."
            )
        return apply_to_function

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

    def process(self, *_arguments: object, **_keyword_arguments: object) -> NoReturn:
        """Refuse the retired `@node.process` with what replaces it: bare `@node` on a class of typed pins."""
        _refuse_the_retired(_RETIRED_PROCESS_NODE_FORM, _arguments)

    def __repr__(self) -> str:
        return "node"


node = NodeDeclarationDecorator()
"""Declares a node by its kind: bare `@node` for a function or a class whose pins each state their pin type, `@node.shader` for a Slang function over Images, `node.rust_function` for a function a module wrote in Rust. `@node.meta` declares what describes it."""

_RETIRED_FORMS_BY_DECORATOR: "dict[str, RetiredDeclaration]" = {
    retired.decorator: retired for retired in RETIRED_DECLARATIONS
}
_RETIRED_FUNCTION_NODE_FORM = _RETIRED_FORMS_BY_DECORATOR["tatolab.stream.node.function"]
_RETIRED_PROCESS_NODE_FORM = _RETIRED_FORMS_BY_DECORATOR["tatolab.stream.node.process"]


def _refuse_the_retired(retired: RetiredDeclaration, arguments: "tuple[object, ...]") -> NoReturn:
    """Refuse a node declared in the `retired` form, naming it when the form was applied to it."""
    declared = arguments[0] if arguments else None
    node_type = getattr(declared, "__name__", None) if declared is not None else None
    raise NodeDeclarationRefusal(
        the_words_refusing_a_retired_declaration(node_type, retired),
        finding_code=RETIRED_DECLARATION_FINDING_CODE,
        node=node_type or retired.spelling,
    )


def _declare_a_node_function(
    function: "Callable[..., Any]",
    *,
    execution: Optional[str],
    interval_ms: int,
    scheduling: Optional[str],
    paced_by_real_time: bool,
) -> NodeFunction:
    """`function` read into a node written as one function, stamped as a class node is."""
    if not inspect.isfunction(function):
        raise NodeDeclarationRefusal(
            f"`@node(...)` declares a class or a function defined with `def`, got {function!r}. "
            f"To fix: put it on a `def` or a class.",
            finding_code="node_function_not_a_plain_def",
            node=getattr(function, "__name__", type(function).__name__),
        )
    if (
        inspect.iscoroutinefunction(function)
        or inspect.isasyncgenfunction(function)
        or inspect.isgeneratorfunction(function)
    ):
        raise NodeDeclarationRefusal(
            f"`{function.__name__}` is not a plain `def`, and a node function's body runs once "
            f"and returns the function the node calls each frame. To fix: make it a plain `def`, "
            f"with no `async` and no `yield`.",
            finding_code="node_function_not_a_plain_def",
            node=function.__name__,
        )
    input_ports, output_ports, members, full_access_context_parameter = (
        read_node_function_ports_and_members(function)
    )
    node_function = NodeFunction(
        function,
        members=members,
        full_access_context_parameter=full_access_context_parameter,
    )
    held_meta = take_node_meta_held_for_the_kind_decorator(function)
    if held_meta:
        hold_node_meta_for_the_kind_decorator(node_function, held_meta)
    return _stamp_a_process_node(
        node_function,
        input_ports=input_ports,
        output_ports=output_ports,
        pin_groups=[],
        execution=execution,
        interval_ms=interval_ms,
        scheduling=scheduling,
        paced_by_real_time=paced_by_real_time,
    )


def _stamp_a_process_node(
    node_class: _ProcessNodeUnderDeclaration,
    *,
    input_ports: "list[dict[str, Any]]",
    output_ports: "list[dict[str, Any]]",
    pin_groups: "list[dict[str, Any]]",
    execution: Optional[str],
    interval_ms: int,
    scheduling: Optional[str],
    paced_by_real_time: bool,
) -> _ProcessNodeUnderDeclaration:
    """Stamp `node_class`, a class or a node function, as a node of `input_ports`, `output_ports`
    and `pin_groups`, which runs in a process of its own."""
    declared_meta = take_node_meta_held_for_the_kind_decorator(node_class)
    conversion = the_conversion_a_node_states(
        getattr(node_class, "__name__"),
        declared_meta,
        declared_by="@node",
        kind=PROCESS_NODE_KIND,
        input_ports=input_ports,
        output_ports=output_ports,
        pin_groups=pin_groups,
        a_node_function=isinstance(node_class, NodeFunction),
    )

    setattr(node_class, NODE_DECLARATION_DECLARED_STAMP, True)
    setattr(node_class, NODE_DECLARATION_KIND_STAMP, PROCESS_NODE_KIND)
    setattr(node_class, NODE_DECLARATION_DESCRIPTION_STAMP, declared_meta.get("description", ""))
    setattr(node_class, NODE_DECLARATION_CATEGORY_STAMP, declared_meta.get("category"))
    setattr(node_class, NODE_DECLARATION_CONVERSION_STAMP, conversion)
    setattr(node_class, NODE_DECLARATION_DECLARED_BY_STAMP, "@node")
    setattr(
        node_class,
        NODE_DECLARATION_EXECUTION_STAMP,
        _resolve_execution(
            execution,
            interval_ms,
            node_class,
            has_input_ports=bool(input_ports)
            or any(pin_group["inputs"] for pin_group in pin_groups),
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
    setattr(node_class, NODE_DECLARATION_PIN_GROUPS_STAMP, pin_groups)

    return node_class


def _resolve_execution(
    execution: Optional[str],
    interval_ms: int,
    node_class: object,
    *,
    has_input_ports: bool,
) -> "dict[str, Any]":
    if execution is None:
        if not has_input_ports:
            raise ValueError(
                f"{getattr(node_class, '__name__', node_class)} declares no input ports, so it must declare "
                f"an execution mode: `@node(execution=\"continuous\", interval_ms=…)` "
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
