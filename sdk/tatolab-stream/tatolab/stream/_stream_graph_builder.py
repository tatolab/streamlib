# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A graph generator: a `@graph_generator` function over a `StreamBuilder`, run to the graph it builds.

Pure Python — the standard library, the name cast and the built-in node base,
nothing native — so a generator's module imports and runs with no engine in the
process. The graph `run_graph_generator` returns is the one `tatolabd` loads,
handed over as JSON; a generator another one places is stored inside it as a
fragment.
"""

from __future__ import annotations

import contextvars
import copy
import inspect
import math
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from dataclasses import dataclass, field
from types import FunctionType
from typing import TYPE_CHECKING, Any, Protocol, TypeGuard, TypeVar, overload

from ._built_in_node import BuiltInNode, ConfigTypeOfTheBuiltInNode
from ._stream_clock_spec import (
    RealTimeStreamClock,
    ScaledStreamClock,
    SteppedStreamClock,
    StreamClock,
)
from ._exposed_name_cast import (
    EXPOSED_NAME_MAXIMUM_LENGTH,
    cast_exposed_name_to_url_safe,
)
from ._function_node_declaration import FunctionNode
from ._node_declaration import NODE_DECLARATION_DECLARED_STAMP
from ._port_family_declaration import PortFamily

__all__ = [
    "GraphFragmentReference",
    "NodeInputPortReference",
    "NodeOutputPortReference",
    "NodeReference",
    "NodeReferenceToNodeType",
    "SoleReferrerNodeReferenceToNodeType",
    "StreamBuilder",
    "graph_generator",
    "is_graph_generator",
    "place_graph_generator",
    "run_graph_generator",
]


_GraphGeneratorFunction = TypeVar(
    "_GraphGeneratorFunction", bound="Callable[..., object]"
)

_GRAPH_GENERATOR_IDENTITY_ATTRIBUTE = "__streamlib_graph_generator_identity__"
_GRAPH_GENERATOR_NAME_ATTRIBUTE = "__streamlib_graph_generator_name__"
_GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE = "__streamlib_graph_generator_description__"

# The class attribute a built-in node carries the path it is registered under in.
_BUILT_IN_NODE_TYPE_ATTRIBUTE = "type"

# The import paths the graph document names the interface nodes by: a fragment's ports.
_GRAPH_INPUT_NODE_TYPE = "tatolab.stream:GraphInput"
_GRAPH_OUTPUT_NODE_TYPE = "tatolab.stream:GraphOutput"
_GRAPH_INTERFACE_PORT_CONFIG_KEY = "port"

# The module name CPython gives the file it was launched with; a child process
# importing this name gets its own entry file instead.
_ENTRY_FILE_MODULE = "__main__"
# CPython puts this in `__qualname__` for anything defined inside a function.
_FUNCTION_LOCAL_MARKER = "<locals>"

# The engine reads a config integer as an i64, else a u64
# (`python_bag_conversion.rs`); a graph carries no wider one.
_SMALLEST_INTEGER_A_GRAPH_CARRIES = -(2**63)
_LARGEST_INTEGER_A_GRAPH_CARRIES = 2**64 - 1

# `tatolab` and `tatolabd` read JSON under serde_json's default recursion limit,
# which parses at most 127 nested containers, and `tatolab` reads the graph inside
# the compile document, one container deeper than its root.
_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT = 127 - 1
# The graph dict, its `nodes` list and the node dict enclose every config.
_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF = _MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT - 3

_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS = (
    "@graph_generator takes no arguments: the name is the function's. Write "
    "`@graph_generator` bare above `def main(stream_builder: StreamBuilder) -> None:`, "
    "each parameter after the builder a setting the generator promotes."
)

# The keywords `StreamBuilder.add` reads itself, which no generator may promote.
_KEYWORDS_ADD_TAKES_ITSELF = ("name", "config")

# The generators running in this run, outermost first, by name and identity.
_GRAPH_GENERATORS_RUNNING: contextvars.ContextVar[tuple[tuple[str, str], ...]] = (
    contextvars.ContextVar("_GRAPH_GENERATORS_RUNNING", default=())
)

# A type checker sees the one-argument signature and flags `@graph_generator()`
# and `@graph_generator(name=...)` where they are written; the call itself
# accepts them only to refuse them by name.
if TYPE_CHECKING:

    def graph_generator(
        graph_generator_function: _GraphGeneratorFunction, /
    ) -> _GraphGeneratorFunction:
        """Mark a module-level function of a `StreamBuilder` and its promoted settings as a graph generator; return it unchanged."""
        ...

else:

    def graph_generator(graph_generator_function=None, /, **misused_keyword_arguments):
        """Mark a module-level function of a `StreamBuilder` and its promoted settings as a graph generator; return it unchanged."""
        if graph_generator_function is None or misused_keyword_arguments:
            raise TypeError(_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS)
        return _stamp_graph_generator(graph_generator_function)


def _stamp_graph_generator(candidate: object) -> FunctionType:
    generator_function = _the_function_unless_it_cannot_be_a_graph_generator(candidate)
    module = generator_function.__module__
    qualname = generator_function.__qualname__
    setattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE, f"{module}:{qualname}")
    setattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE, generator_function.__name__)
    setattr(
        generator_function,
        _GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE,
        inspect.getdoc(generator_function) or "",
    )
    return generator_function


def _the_function_unless_it_cannot_be_a_graph_generator(candidate: object) -> FunctionType:
    if not inspect.isfunction(candidate):
        # What a class-member decorator returns: a descriptor, callable only for
        # `staticmethod`.
        if isinstance(candidate, staticmethod) or (
            hasattr(type(candidate), "__get__") and not callable(candidate)
        ):
            descriptor_type_name = _type_name_as_written(candidate)
            raise TypeError(
                f"@graph_generator decorates a plain module-level function taking a "
                f"`StreamBuilder`, and {candidate!r} is of type `{descriptor_type_name}`, "
                f"which makes a class member of what it wraps. Put `@graph_generator` on a "
                f"plain module-level `def`, with no `@{descriptor_type_name}` under it."
            )
        # Stacked over a `def` or a class `@graph_generator` receives something
        # callable, and over a class-member decorator the descriptor above, so any
        # other value reached it as an argument: `@graph_generator("rig")`.
        sentence_naming_the_bare_form_when_called_with_a_value = (
            "" if callable(candidate) else f" {_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS}"
        )
        raise TypeError(
            f"@graph_generator decorates a plain module-level function taking a "
            f"`StreamBuilder`, and {candidate!r} is not one."
            f"{sentence_naming_the_bare_form_when_called_with_a_value}"
        )
    generator_function = candidate
    module = generator_function.__module__
    qualname = generator_function.__qualname__
    if generator_function.__name__ == "<lambda>":
        raise TypeError(
            f"a graph generator is a module-level function defined with `def`; "
            f"`{module}:{qualname}` is a lambda, which has no name to load it by"
        )
    if (
        inspect.iscoroutinefunction(generator_function)
        or inspect.isgeneratorfunction(generator_function)
        or inspect.isasyncgenfunction(generator_function)
    ):
        raise TypeError(
            f"`{module}:{qualname}` is an `async def` or generator function, so calling it "
            f"over the builder runs none of its body. A graph generator is a plain `def` "
            f"that adds its nodes when called."
        )
    if _FUNCTION_LOCAL_MARKER in qualname or "." in qualname:
        raise TypeError(
            f"a graph generator is a module-level function, and `{module}:{qualname}` is "
            f"defined inside a "
            f"{'function' if _FUNCTION_LOCAL_MARKER in qualname else 'class'}, so nothing "
            f"can load it by name. Move `{generator_function.__name__}` to module scope."
        )
    parameters = list(inspect.signature(generator_function).parameters.values())
    takes_the_builder_first = bool(parameters) and parameters[0].kind in (
        inspect.Parameter.POSITIONAL_ONLY,
        inspect.Parameter.POSITIONAL_OR_KEYWORD,
    )
    promoted_settings_are_named = all(
        parameter.kind
        in (inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY)
        for parameter in parameters[1:]
    )
    if not (takes_the_builder_first and promoted_settings_are_named):
        raise TypeError(
            f"a graph generator takes the `StreamBuilder` it builds on as its one "
            f"positional parameter, then each setting it promotes by name — "
            f"`def {generator_function.__name__}(stream_builder: StreamBuilder, "
            f"radius: int = 4) -> None:`; `{module}:{qualname}` is "
            f"`{generator_function.__name__}{inspect.signature(generator_function)}`"
        )
    settings_add_takes_itself = [
        parameter.name
        for parameter in parameters[1:]
        if parameter.name in _KEYWORDS_ADD_TAKES_ITSELF
    ]
    if settings_add_takes_itself:
        raise TypeError(
            f"graph generator `{module}:{qualname}` promotes "
            f"{', '.join(f'`{name}`' for name in settings_add_takes_itself)}, which "
            f"`stream_builder.add` takes itself — the fragment's name and a node's config — "
            f"so no placement could set it. Give the setting another name."
        )
    return generator_function


def is_graph_generator(candidate: object) -> TypeGuard[Callable[..., Any]]:
    """Whether `candidate` is a function `@graph_generator` declared."""
    return inspect.isfunction(candidate) and all(
        isinstance(getattr(candidate, stamp, None), str)
        for stamp in (
            _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE,
            _GRAPH_GENERATOR_NAME_ATTRIBUTE,
            _GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE,
        )
    )


@contextmanager
def _running_the_graph_generator(generator_function: Callable[..., Any]) -> Iterator[None]:
    """Mark `generator_function` running for as long as the block runs, refusing one already running higher in this run."""
    identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
    name: str = getattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE)
    running = _GRAPH_GENERATORS_RUNNING.get()
    if any(running_identity == identity for _, running_identity in running):
        chain = " → ".join([*(running_name for running_name, _ in running), name])
        raise ValueError(
            f"graph generator `{identity}` places itself: {chain}. A generator already "
            f"running higher in this run is never placed again, so the graph it builds "
            f"ends; nothing of this run is kept"
        )
    token = _GRAPH_GENERATORS_RUNNING.set((*running, (name, identity)))
    try:
        yield
    finally:
        _GRAPH_GENERATORS_RUNNING.reset(token)


class _NodeClassConstructedWithoutConfig(Protocol):
    """A `@node.process` class its interpreter constructs as `cls()`."""

    def __call__(self) -> object: ...


class _NodeClassConstructedWithConfig(Protocol):
    """A `@node.process` class its interpreter constructs as `cls(config=...)`."""

    def __call__(self, *, config: Any) -> object: ...


@dataclass(frozen=True, slots=True)
class NodeOutputPortReference:
    """An output port of a node a `StreamBuilder` holds — a link's producing end; names cast."""

    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class NodeInputPortReference:
    """An input port of a node a `StreamBuilder` holds — a link's consuming end; names cast."""

    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class NodeReference:
    """A node `StreamBuilder.add` recorded, under the cast name links name it by."""

    name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "name", _cast_name(self.name, "node name"))

    def output(self, port_name: str) -> NodeOutputPortReference:
        """Name one of this node's output ports, cast as `@node.process` declared it."""
        return NodeOutputPortReference(self.name, port_name)

    def input(self, port_name: str) -> NodeInputPortReference:
        """Name one of this node's input ports, cast as `@node.process` declared it."""
        return NodeInputPortReference(self.name, port_name)


@dataclass(frozen=True, slots=True)
class GraphFragmentReference(NodeReference):
    """A fragment `StreamBuilder.add` placed: its ports are the Inputs and Outputs its generator's interface nodes declare."""

    input_port_names: tuple[str, ...]
    output_port_names: tuple[str, ...]

    def output(self, port_name: str) -> NodeOutputPortReference:
        """Name one of the Outputs the fragment's `GraphOutput` nodes declare."""
        return NodeOutputPortReference(
            self.name, self._declared_port(port_name, self.output_port_names, "Output")
        )

    def input(self, port_name: str) -> NodeInputPortReference:
        """Name one of the Inputs the fragment's `GraphInput` nodes declare."""
        return NodeInputPortReference(
            self.name, self._declared_port(port_name, self.input_port_names, "Input")
        )

    def _declared_port(
        self, port_name: str, declared_port_names: tuple[str, ...], port_kind: str
    ) -> str:
        cast = _cast_name(port_name, "port name")
        if cast not in declared_port_names:
            declared = ", ".join(f"`{name}`" for name in declared_port_names) or "none"
            raise ValueError(
                f"fragment `{self.name}` declares no {port_kind} `{cast}`; its graph "
                f"generator's {port_kind}s are: {declared}"
            )
        return cast


type NodeReferenceToNodeType[NodeClassTheReferenceNames] = NodeReference
"""A config field holding a reference to a node of `NodeClassTheReferenceNames`, checked at load."""

type SoleReferrerNodeReferenceToNodeType[NodeClassTheReferenceNames] = NodeReference
"""A config field holding a reference to a node of `NodeClassTheReferenceNames` that no other node names under the same key, checked at load."""


@dataclass(frozen=True)
class _RecordedNode:
    name: str
    node_type: str
    config: dict[str, Any]
    typed_name: str | None
    class_short_name: str
    fragment: dict[str, Any] | None = field(default=None)

    def as_graph_document_node(self) -> dict[str, Any]:
        node: dict[str, Any] = {
            "name": self.name,
            "type": self.node_type,
            "config": copy.deepcopy(self.config),
        }
        if self.fragment is not None:
            node["fragment"] = copy.deepcopy(self.fragment)
        return node

    def how_its_name_was_given(self) -> str:
        if self.typed_name is not None:
            return f"typed as {self.typed_name!r}"
        return f"the default name of {self.class_short_name}"


class StreamBuilder:
    """The builder a `@graph_generator` function adds and links nodes on; it runs nothing."""

    def __init__(self, name: str, *, placed_as_a_fragment: bool = False) -> None:
        self._name = _cast_name(name, "stream name")
        self._placed_as_a_fragment = placed_as_a_fragment
        self._recorded_nodes_by_name: dict[str, _RecordedNode] = {}
        self._recorded_links: list[dict[str, dict[str, str]]] = []
        self._stream_clock: StreamClock | None = None

    @property
    def name(self) -> str:
        """The name of the stream or fragment being built, cast."""
        return self._name

    @overload
    def add(
        self,
        node_class: _GraphGeneratorPlaced,
        *,
        name: str | None = None,
        config: None = None,
        **promoted_settings: Any,
    ) -> GraphFragmentReference: ...

    @overload
    def add(
        self,
        node_class: FunctionNode[..., Any],
        *,
        name: str | None = None,
        config: Mapping[str, Any] | None = None,
    ) -> NodeReference: ...

    @overload
    def add(
        self,
        node_class: _NodeClassConstructedWithoutConfig
        | _NodeClassConstructedWithConfig,
        *,
        name: str | None = None,
        config: Mapping[str, Any] | None = None,
    ) -> NodeReference: ...

    @overload
    def add(
        self,
        node_class: type[BuiltInNode[ConfigTypeOfTheBuiltInNode]],
        *,
        name: str | None = None,
        config: ConfigTypeOfTheBuiltInNode | None = None,
    ) -> NodeReference: ...

    def add(
        self,
        node_class: object,
        *,
        name: str | None = None,
        config: Mapping[str, Any] | None = None,
        **promoted_settings: Any,
    ) -> NodeReference:
        """Record a node of `node_class` — a node class, a `@node.function`, or a `@graph_generator` run as a fragment over `promoted_settings` — under `name` (cast) or its short name, cast."""
        if is_graph_generator(node_class):
            return self._place_the_graph_generator(
                node_class, name=name, config=config, promoted_settings=promoted_settings
            )
        if promoted_settings:
            raise TypeError(
                f"keyword arguments besides `name` and `config` are a graph generator's "
                f"promoted settings, and {node_class!r} is a node, not a "
                f"`@graph_generator`: pass its settings as `config={{...}}`. Got "
                f"{', '.join(f'`{key}`' for key in promoted_settings)}."
            )
        if not isinstance(node_class, (type, FunctionNode)):
            raise TypeError(_not_a_node_refusal(node_class))
        node_type = _node_type_of(node_class)
        node_names_the_config_references: list[str] = []
        config_as_json_object = _config_as_json_object(
            config, node_names_the_config_references
        )
        for referenced_node_name in node_names_the_config_references:
            self._refuse_a_node_this_stream_does_not_hold(referenced_node_name)
        node_name = (
            self._name_a_defaulted_node_takes(node_class)
            if name is None
            else self._typed_name_unless_taken(name)
        )
        recorded_node = _RecordedNode(
            name=node_name,
            node_type=node_type,
            config=config_as_json_object,
            typed_name=name,
            class_short_name=node_class.__name__,
        )
        self._recorded_nodes_by_name[node_name] = recorded_node
        return NodeReference(node_name)

    def _place_the_graph_generator(
        self,
        generator_function: Callable[..., Any],
        *,
        name: str | None,
        config: Mapping[str, Any] | None,
        promoted_settings: dict[str, Any],
    ) -> GraphFragmentReference:
        identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
        if config is not None:
            raise TypeError(
                f"graph generator `{identity}` takes its promoted settings as keyword "
                f"arguments of `add`, not `config`: "
                f"`stream_builder.add({generator_function.__name__}, radius=8)`"
            )
        _refuse_settings_the_generator_does_not_promote(generator_function, promoted_settings)
        node_names_the_settings_reference: list[str] = []
        promoted_config = _config_as_json_object(
            promoted_settings, node_names_the_settings_reference
        )
        if node_names_the_settings_reference:
            raise TypeError(
                f"graph generator `{identity}` is promoted a `NodeReference`, and a node of "
                f"this graph is not a node of the fragment: promote the setting the node "
                f"would be built from instead"
            )
        node_name = (
            self._name_a_defaulted_node_takes(generator_function)
            if name is None
            else self._typed_name_unless_taken(name)
        )
        fragment_builder = StreamBuilder(node_name, placed_as_a_fragment=True)
        with _running_the_graph_generator(generator_function):
            generator_function(fragment_builder, **promoted_settings)
        fragment = fragment_builder._compiled_fragment()
        self._recorded_nodes_by_name[node_name] = _RecordedNode(
            name=node_name,
            node_type=identity,
            config=promoted_config,
            typed_name=name,
            class_short_name=generator_function.__name__,
            fragment=fragment,
        )
        return GraphFragmentReference(
            node_name,
            input_port_names=fragment_builder._interface_port_names(_GRAPH_INPUT_NODE_TYPE),
            output_port_names=fragment_builder._interface_port_names(_GRAPH_OUTPUT_NODE_TYPE),
        )

    def connect(
        self, source: NodeOutputPortReference, destination: NodeInputPortReference
    ) -> None:
        """Record a link from `source` to `destination`, both ports of nodes this stream added."""
        if not isinstance(source, NodeOutputPortReference):
            raise TypeError(
                f"connect's source must name an output port of a node this stream "
                f"added: `stream_builder.add(...).output(port_name)`. A link's ends are "
                f"both in this stream; read another stream's track at a relay, on this "
                f"machine or another, through a `MoqTrackReceive` node this stream adds. "
                f"Got {source!r}."
            )
        if not isinstance(destination, NodeInputPortReference):
            raise TypeError(
                f"connect's destination must name an input port of a node this stream "
                f"added: `stream_builder.add(...).input(port_name)`. Got {destination!r}."
            )
        self._recorded_links.append(
            {
                "source": self._link_end(source),
                "target": self._link_end(destination),
            }
        )

    def run_on_clock(self, stream_clock: StreamClock) -> None:
        """Run this stream on `stream_clock` instead of real time; called at most once."""
        if not isinstance(
            stream_clock, (RealTimeStreamClock, ScaledStreamClock, SteppedStreamClock)
        ):
            raise TypeError(
                f"run_on_clock takes a `RealTimeStreamClock()`, a `ScaledStreamClock(scale=…)` "
                f"or a `SteppedStreamClock(step_ns=…)`, imported from `tatolab.stream`. "
                f"Got {stream_clock!r}."
            )
        if self._placed_as_a_fragment:
            raise ValueError(
                f"fragment `{self._name}` is placed in a graph that names its own clock; a "
                f"generator names a clock only when it fills a stream"
            )
        if self._stream_clock is not None:
            raise ValueError(
                f"stream `{self._name}` already runs on {self._stream_clock!r}; a stream "
                f"names its clock once"
            )
        self._stream_clock = stream_clock

    def _typed_name_unless_taken(self, typed_name: str) -> str:
        cast = _cast_name(typed_name, "node name")
        node_already_holding_the_cast_name = self._recorded_nodes_by_name.get(cast)
        if node_already_holding_the_cast_name is not None:
            raise ValueError(
                f"the node name {typed_name!r} casts to `{cast}`, which node `{cast}` in "
                f"stream `{self._name}` already has — "
                f"{node_already_holding_the_cast_name.how_its_name_was_given()}. "
                f"A name the author gives is an address, so it is never suffixed: give one "
                f"of them another name, or leave the name out to take a `-2` suffix."
            )
        return cast

    def _name_a_defaulted_node_takes(
        self, node_class: type | FunctionNode[..., Any] | Callable[..., Any]
    ) -> str:
        cast = cast_exposed_name_to_url_safe(node_class.__name__)
        if cast not in self._recorded_nodes_by_name:
            return cast
        ordinal = 2
        while True:
            suffix = f"-{ordinal}"
            stem = cast[: EXPOSED_NAME_MAXIMUM_LENGTH - len(suffix)].rstrip("-")
            candidate = f"{stem}{suffix}"
            if candidate not in self._recorded_nodes_by_name:
                return candidate
            ordinal += 1

    def _refuse_a_node_this_stream_does_not_hold(self, node_name: str) -> None:
        if node_name not in self._recorded_nodes_by_name:
            held = ", ".join(sorted(self._recorded_nodes_by_name)) or "no node"
            raise ValueError(
                f"node `{node_name}` is not in stream `{self._name}`, which holds: {held}. "
                f"Name a port through the reference this stream's `add` returned."
            )

    def _link_end(
        self, end: NodeOutputPortReference | NodeInputPortReference
    ) -> dict[str, str]:
        self._refuse_a_node_this_stream_does_not_hold(end.node_name)
        return {"node": end.node_name, "port": end.port_name}

    def _interface_port_names(self, interface_node_type: str) -> tuple[str, ...]:
        return tuple(
            _cast_name(str(recorded_node.config[_GRAPH_INTERFACE_PORT_CONFIG_KEY]), "port name")
            for recorded_node in self._recorded_nodes_by_name.values()
            if recorded_node.node_type == interface_node_type
            and isinstance(recorded_node.config.get(_GRAPH_INTERFACE_PORT_CONFIG_KEY), str)
        )

    def _compiled_fragment(self) -> dict[str, Any]:
        return {
            "nodes": [
                recorded_node.as_graph_document_node()
                for recorded_node in self._recorded_nodes_by_name.values()
            ],
            "links": copy.deepcopy(self._recorded_links),
        }

    def _compiled_graph(self) -> dict[str, Any]:
        clock = (
            {}
            if self._stream_clock is None
            else {"clock": self._stream_clock.to_graph_document()}
        )
        return {"stream": self._name, **clock, **self._compiled_fragment()}


class _GraphGeneratorPlaced(Protocol):
    """A `@graph_generator` function `StreamBuilder.add` places as a fragment."""

    def __call__(self, stream_builder: StreamBuilder, /, *args: Any, **kwargs: Any) -> object: ...


def run_graph_generator(
    generator_function: Callable[..., object],
    *,
    promoted_settings: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Run a `@graph_generator` function once over a fresh `StreamBuilder`, filling a stream, and return the graph it built."""
    if not is_graph_generator(generator_function):
        raise TypeError(
            f"{generator_function!r} is not a graph generator: decorate it with "
            f"@graph_generator — a module-level `def` taking a `StreamBuilder`"
        )
    settings = dict(promoted_settings or {})
    _refuse_settings_the_generator_does_not_promote(generator_function, settings)
    stream_builder = StreamBuilder(getattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE))
    with _running_the_graph_generator(generator_function):
        generator_function(stream_builder, **settings)
    graph = stream_builder._compiled_graph()
    _refuse_a_fragment_nested_deeper_than_a_graph_document_reads(graph)
    return graph


def place_graph_generator(
    generator_function: Callable[..., object],
    *,
    name: str | None = None,
    promoted_settings: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Run a `@graph_generator` function as a fragment placed under `name` (cast) or its own name, and return the fragment node a graph document stores: its `name`, its generator as `type`, its promoted settings as `config` and its `fragment`."""
    if not is_graph_generator(generator_function):
        raise TypeError(
            f"{generator_function!r} is not a graph generator: decorate it with "
            f"@graph_generator — a module-level `def` taking a `StreamBuilder`"
        )
    placing_builder = StreamBuilder(getattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE))
    placed = placing_builder.add(
        generator_function, name=name, **dict(promoted_settings or {})
    )
    fragment_node = placing_builder._recorded_nodes_by_name[placed.name].as_graph_document_node()
    _refuse_a_fragment_nested_deeper_than_a_graph_document_reads({"nodes": [fragment_node]})
    return fragment_node


def _refuse_settings_the_generator_does_not_promote(
    generator_function: Callable[..., Any], promoted_settings: Mapping[str, Any]
) -> None:
    identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
    parameters = list(inspect.signature(generator_function).parameters.values())[1:]
    promoted = {parameter.name: parameter for parameter in parameters}
    unknown = [key for key in promoted_settings if key not in promoted]
    missing = [
        parameter.name
        for parameter in parameters
        if parameter.default is inspect.Parameter.empty
        and parameter.name not in promoted_settings
    ]
    if not (unknown or missing):
        return
    promoted_listed = ", ".join(f"`{name}`" for name in promoted) or "none"
    what_is_wrong = []
    if unknown:
        what_is_wrong.append(
            f"it promotes no {', '.join(f'`{key}`' for key in unknown)}"
        )
    if missing:
        what_is_wrong.append(
            f"{', '.join(f'`{name}`' for name in missing)} has no default and is not given"
        )
    raise TypeError(
        f"graph generator `{identity}`: {'; '.join(what_is_wrong)}. The settings it "
        f"promotes are: {promoted_listed}"
    )


def _refuse_a_fragment_nested_deeper_than_a_graph_document_reads(graph: dict[str, Any]) -> None:
    deepest_containers, deepest_fragment_path = _containers_deep_and_the_deepest_fragment(
        graph, 1, ""
    )
    if deepest_containers <= _MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT or not deepest_fragment_path:
        return
    raise ValueError(
        f"fragment `{deepest_fragment_path}` nests {deepest_containers} containers deep from "
        f"the graph's root, and a graph document nests at most "
        f"{_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT}: each fragment a generator places "
        f"nests three deeper. Place fewer generators inside one another"
    )


def _containers_deep_and_the_deepest_fragment(
    value: object, containers_deep: int, fragment_path: str
) -> tuple[int, str]:
    deepest = (containers_deep, fragment_path)
    if isinstance(value, dict):
        children: list[tuple[object, str]] = []
        for key, child in value.items():
            if key == "fragment" and isinstance(value.get("name"), str):
                path = f"{fragment_path}/{value['name']}" if fragment_path else value["name"]
                children.append((child, path))
            else:
                children.append((child, fragment_path))
    elif isinstance(value, list):
        children = [(child, fragment_path) for child in value]
    else:
        return deepest
    for child, child_path in children:
        if isinstance(child, (dict, list)):
            candidate = _containers_deep_and_the_deepest_fragment(
                child, containers_deep + 1, child_path
            )
            if candidate[0] > deepest[0]:
                deepest = candidate
    return deepest


def _cast_name(name: str, what_the_name_names: str) -> str:
    if not isinstance(name, str):
        raise TypeError(
            f"a {what_the_name_names} is a str; got {name!r}, of type "
            f"`{_type_name_as_written(name)}` — pass the name as a str"
        )
    return cast_exposed_name_to_url_safe(name)


def _not_a_node_refusal(node_class: object) -> str:
    return (
        f"{node_class!r} is not a node: decorate the class with `@node.process` or the "
        f"function with `@node.function`, and pass it itself rather than an instance of "
        f"it or a call of it"
    )


def _node_type_of(node_class: type | FunctionNode[..., Any]) -> str:
    if hasattr(node_class, NODE_DECLARATION_DECLARED_STAMP):
        return _node_class_import_path(node_class)
    built_in_node_type = getattr(node_class, _BUILT_IN_NODE_TYPE_ATTRIBUTE, None)
    if isinstance(built_in_node_type, str):
        return built_in_node_type
    raise TypeError(_not_a_node_refusal(node_class))


def _node_class_import_path(node_class: type | FunctionNode[..., Any]) -> str:
    module = node_class.__module__
    qualname = node_class.__qualname__
    # Before the entry-file check: a class inside a function is unimportable
    # wherever its module is, so the entry-file fix would not help it.
    if _FUNCTION_LOCAL_MARKER in qualname:
        raise ValueError(
            f"node `{module}:{qualname}` is defined inside a function, so it identifies "
            f"as a name no interpreter can import — `{_FUNCTION_LOCAL_MARKER}` marks a "
            f"class that exists only for the duration of a call. Every Python node runs "
            f"in its own child process, which reaches the class by importing this "
            f"name.\n\n"
            f"Move the class to module scope. If it was parameterised by the enclosing "
            f"function's arguments, pass those through `stream_builder.add(..., config={{...}})` "
            f"instead — config reaches the child, a closure cannot."
        )
    if module == _ENTRY_FILE_MODULE and isinstance(node_class, FunctionNode):
        module_suggestion = _suggested_module_name(qualname)
        raise ValueError(
            f"function node `{qualname}` is defined in the entry file, so it identifies as "
            f"`__main__:{qualname}` — a name no other interpreter can import. A function "
            f"node runs inside the read of the node it feeds, in that node's process, which "
            f"imports the function by this name and would get its own entry file instead.\n\n"
            f"Move `{qualname}` into an importable module beside the entry file and "
            f"import it from there — one import line:\n\n"
            f"    # {module_suggestion}.py\n"
            f"    @node.function\n"
            f"    def {qualname}(...): ...\n\n"
            f"    # stream.py\n"
            f"    from {module_suggestion} import {qualname}\n\n"
            f"The entry file itself may still run as `__main__`; only nodes may not live "
            f"in it."
        )
    if module == _ENTRY_FILE_MODULE:
        module_suggestion = _suggested_module_name(qualname)
        raise ValueError(
            f"node `{qualname}` is defined in the entry file, so it identifies as "
            f"`__main__:{qualname}` — a name no other interpreter can import. Every "
            f"Python node runs in its own child process, which imports the class by "
            f"this name and would get its own entry file instead.\n\n"
            f"Move `{qualname}` into an importable module beside the entry file and "
            f"import it from there — one import line:\n\n"
            f"    # {module_suggestion}.py\n"
            f"    @node.process(...)\n"
            f"    class {qualname}: ...\n\n"
            f"    # stream.py\n"
            f"    from {module_suggestion} import {qualname}\n\n"
            f"The entry file itself may still run as `__main__`; only node classes may "
            f"not live in it."
        )
    return f"{module}:{qualname}"


def _suggested_module_name(qualname: str) -> str:
    leaf = qualname.rsplit(".", 1)[-1]
    snake_characters: list[str] = []
    for position, character in enumerate(leaf):
        if "A" <= character <= "Z":
            if position != 0:
                snake_characters.append("_")
            snake_characters.append(character.lower())
        else:
            snake_characters.append(character)
    return "".join(snake_characters) or "nodes"


def _config_as_json_object(
    config: Mapping[str, Any] | None, node_names_the_config_references: list[str]
) -> dict[str, Any]:
    if config is None:
        return {}
    if not isinstance(config, Mapping):
        raise TypeError(
            f"config must be a mapping of str keys to JSON values — the object a node's "
            f"config is; got {config!r}, of type `{_type_name_as_written(config)}`"
        )
    return _json_object(
        config, "config", {id(config): "config"}, node_names_the_config_references
    )


def _json_object(
    mapping: Mapping[Any, Any],
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
    node_names_the_config_references: list[str],
) -> dict[str, Any]:
    json_object: dict[str, Any] = {}
    for key, value in mapping.items():
        if not isinstance(key, str):
            raise TypeError(
                f"config must be JSON: `{key_path}` has the key {key!r}, of type "
                f"`{_type_name_as_written(key)}`, and a JSON object's keys are strings — "
                f"convert it with `str(...)`"
            )
        plain_key = str.__str__(key)
        if plain_key in json_object:
            raise ValueError(
                f"config must be JSON: `{key_path}` has two keys that are both "
                f"{plain_key!r} as plain strings, and a JSON object holds a key once — "
                f"keep one of them, or rename the other"
            )
        json_object[plain_key] = _json_value(
            value,
            f"{key_path}[{plain_key!r}]",
            key_paths_of_enclosing_containers_by_id,
            node_names_the_config_references,
        )
    return json_object


def _json_value(
    value: object,
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
    node_names_the_config_references: list[str],
) -> Any:
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, NodeReference):
        _refuse_a_container_nested_too_deep(key_path, key_paths_of_enclosing_containers_by_id)
        node_names_the_config_references.append(value.name)
        return {"node": value.name}
    if isinstance(value, PortFamily):
        return value._wire_name  # pyright: ignore[reportPrivateUsage]
    if isinstance(value, str):
        return str.__str__(value)
    if isinstance(value, int):
        return _json_integer(value, key_path)
    if isinstance(value, float):
        return _json_float(value, key_path)
    if not isinstance(value, (Mapping, list, tuple)):
        raise TypeError(
            f"config must be JSON: `{key_path}` is of type `{_type_name_as_written(value)}`, "
            f"and a graph carries only dict, list, tuple, str, int, float, bool, None, a "
            f"port family and the `NodeReference` `add` returned — "
            f"convert it with `bool(...)`, `int(...)`, `float(...)` or `str(...)`, or a "
            f"collection with `list(...)`"
        )
    enclosing_key_path = key_paths_of_enclosing_containers_by_id.get(id(value))
    if enclosing_key_path is not None:
        raise ValueError(
            f"config must be JSON: `{key_path}` is `{enclosing_key_path}`, which holds "
            f"it — a value holding itself has no JSON form. Break the cycle: put the "
            f"data `{key_path}` should carry there, not the container holding it"
        )
    _refuse_a_container_nested_too_deep(key_path, key_paths_of_enclosing_containers_by_id)
    key_paths_of_enclosing_containers_by_id[id(value)] = key_path
    if isinstance(value, Mapping):
        json_container: Any = _json_object(
            value,
            key_path,
            key_paths_of_enclosing_containers_by_id,
            node_names_the_config_references,
        )
    else:
        json_container = [
            _json_value(
                item,
                f"{key_path}[{index}]",
                key_paths_of_enclosing_containers_by_id,
                node_names_the_config_references,
            )
            for index, item in enumerate(value)
        ]
    del key_paths_of_enclosing_containers_by_id[id(value)]
    return json_container


def _refuse_a_container_nested_too_deep(
    key_path: str, key_paths_of_enclosing_containers_by_id: dict[int, str]
) -> None:
    containers_enclosing_this_one_counting_config = len(
        key_paths_of_enclosing_containers_by_id
    )
    if (
        containers_enclosing_this_one_counting_config
        >= _MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF
    ):
        raise ValueError(
            f"config nests too deep for a graph: `{key_path}` is a container "
            f"{containers_enclosing_this_one_counting_config + 1} deep counting `config` "
            f"itself, and a config nests at most "
            f"{_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF} — a graph nests at most "
            f"{_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT} containers from its root, and "
            f"the graph, its `nodes` list and the node enclose every config. Nest the data at most "
            f"{_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF} containers deep, or carry "
            f"the deeper part as a `str`"
        )


def _json_integer(value: int, key_path: str) -> int:
    plain_integer = int.__int__(value)
    if not (
        _SMALLEST_INTEGER_A_GRAPH_CARRIES
        <= plain_integer
        <= _LARGEST_INTEGER_A_GRAPH_CARRIES
    ):
        # The value is left out: CPython refuses to render an int of more than
        # 4300 digits as text.
        raise ValueError(
            f"config integers must fit the graph's 64-bit range: `{key_path}` is outside "
            f"-2**63 to 2**64 - 1; carry a value that large as a `str`"
        )
    return plain_integer


def _type_name_as_written(value: object) -> str:
    value_type = type(value)
    if value_type.__module__ == "builtins":
        return value_type.__qualname__
    return f"{value_type.__module__}.{value_type.__qualname__}"


def _json_float(value: float, key_path: str) -> float:
    plain_float = float.__float__(value)
    if not math.isfinite(plain_float):
        raise ValueError(
            f"config must be JSON: `{key_path}` is {plain_float!r}, which JSON cannot "
            f"carry — pass `None` where there is no value, or carry it as a `str`"
        )
    return plain_float
