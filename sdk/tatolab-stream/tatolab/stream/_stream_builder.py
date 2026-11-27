# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`StreamBuilder`, the node and port references it hands out, and the input values it compiles to JSON.

Pure Python, like the rest of the graph generator: a generator's module imports
and runs with no engine in the process.
"""

from __future__ import annotations

import copy
import math
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Protocol, overload

from ._built_in_node import BuiltInNode, InputValuesOfATypedBuiltInNode
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
from ._node_function import NodeFunction
from ._node_declaration import NODE_DECLARATION_DECLARED_STAMP
from ._typed_node_pins import TypedNodePins

from ._graph_generator_detection import (
    _FUNCTION_LOCAL_MARKER,
    _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE,
    _refuse_settings_the_generator_does_not_promote,
    _running_the_graph_generator,
    _type_name_as_written,
    is_graph_generator,
)
from ._graph_generator_settings import the_settings_a_graph_generator_promotes


# The class attribute a built-in node carries the path it is registered under in.
_BUILT_IN_NODE_TYPE_ATTRIBUTE = "type"

# The import paths the graph document names the interface nodes by: a fragment's ports.
_GRAPH_INPUT_NODE_TYPE = "tatolab.stream:GraphInput"
_GRAPH_OUTPUT_NODE_TYPE = "tatolab.stream:GraphOutput"
_GRAPH_INTERFACE_PORT_CONFIG_KEY = "port"

# The module name CPython gives the file it was launched with; a child process
# importing this name gets its own entry file instead.
_ENTRY_FILE_MODULE = "__main__"

# The engine reads an input value's integer as an i64, else a u64
# (`python_bag_conversion.rs`); a graph carries no wider one.
_SMALLEST_INTEGER_A_GRAPH_CARRIES = -(2**63)
_LARGEST_INTEGER_A_GRAPH_CARRIES = 2**64 - 1

# `tatolab` and `tatolabd` read JSON under serde_json's default recursion limit,
# which parses at most 127 nested containers, and `tatolab` reads the graph inside
# the compile document, one container deeper than its root.
_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT = 127 - 1
# The graph dict, its `nodes` list and the node dict enclose every node's input values.
_MOST_CONTAINERS_INPUT_VALUES_NEST_COUNTING_THEMSELVES = _MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT - 3

# The key a graph document holds a node's value of each input under, by input name.
_NODE_INPUT_VALUES_KEY = "input_values"


class _NodeClassConstructedWithNothing(Protocol):
    """A class node its interpreter constructs as `cls()`."""

    def __call__(self) -> object: ...


@dataclass(frozen=True, slots=True)
class NodeOutputPortReference:
    """An output port of a node a `StreamBuilder` holds — a link's producing end; names cast."""

    node_name: str
    port_name: str
    member_name: str | None = None
    """The member of the port's bundle the link starts at, as the bundle names it; `None` for the whole port."""

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))
        _refuse_a_member_name_that_is_not_a_string(self.member_name)


@dataclass(frozen=True, slots=True)
class NodeInputPortReference:
    """An input port of a node a `StreamBuilder` holds — a link's consuming end; names cast."""

    node_name: str
    port_name: str
    member_name: str | None = None
    """The member of the port's bundle the link ends at, as the bundle names it; `None` for the whole port."""

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))
        _refuse_a_member_name_that_is_not_a_string(self.member_name)


@dataclass(frozen=True, slots=True)
class NodeReference:
    """A node `StreamBuilder.add` recorded, under the cast name links name it by."""

    name: str
    typed_pins: TypedNodePins | None = field(default=None, compare=False, repr=False)
    """The pins the node declares when it is a typed node, which its port names are checked against."""
    input_values: Mapping[str, Any] = field(default_factory=dict, compare=False, repr=False)
    """The node's input values as recorded, which choose the bundle a port's members are checked against."""

    def __post_init__(self) -> None:
        object.__setattr__(self, "name", _cast_name(self.name, "node name"))

    def output(self, port_name: str, *, member: str | None = None) -> NodeOutputPortReference:
        """Name one of this node's output ports, cast; on a typed node, one it declares.

        `member` names one member of the port's bundle, the link starting at it alone; on a
        typed node, one the bundle has.
        """
        if self.typed_pins is not None:
            port_name = self.typed_pins.port_named(_cast_name(port_name, "port name"), "output")
            if member is not None:
                member = self.typed_pins.member_named(port_name, "output", member, self.input_values)
        return NodeOutputPortReference(self.name, port_name, member)

    def input(self, port_name: str, *, member: str | None = None) -> NodeInputPortReference:
        """Name one of this node's input ports, cast; on a typed node, one it declares.

        `member` names one member of the port's bundle, the link ending at it alone; on a
        typed node, one the bundle has.
        """
        if self.typed_pins is not None:
            port_name = self.typed_pins.port_named(_cast_name(port_name, "port name"), "input")
            if member is not None:
                member = self.typed_pins.member_named(port_name, "input", member, self.input_values)
        return NodeInputPortReference(self.name, port_name, member)


@dataclass(frozen=True, slots=True)
class GraphFragmentReference(NodeReference):
    """A fragment `StreamBuilder.add` placed: its ports are the Inputs and Outputs its generator's interface nodes declare."""

    input_port_names: tuple[str, ...] = ()
    output_port_names: tuple[str, ...] = ()

    def output(self, port_name: str, *, member: str | None = None) -> NodeOutputPortReference:
        """Name one of the Outputs the fragment's `GraphOutput` nodes declare; `member` is refused."""
        return NodeOutputPortReference(
            self.name, self._declared_port(port_name, self.output_port_names, "Output", member)
        )

    def input(self, port_name: str, *, member: str | None = None) -> NodeInputPortReference:
        """Name one of the Inputs the fragment's `GraphInput` nodes declare; `member` is refused."""
        return NodeInputPortReference(
            self.name, self._declared_port(port_name, self.input_port_names, "Input", member)
        )

    def _declared_port(
        self,
        port_name: str,
        declared_port_names: tuple[str, ...],
        port_kind: str,
        member: str | None,
    ) -> str:
        cast = _cast_name(port_name, "port name")
        if cast not in declared_port_names:
            declared = ", ".join(f"`{name}`" for name in declared_port_names) or "none"
            raise ValueError(
                f"fragment `{self.name}` declares no {port_kind} `{cast}`; its graph "
                f"generator's {port_kind}s are: {declared}"
            )
        if member is not None:
            raise ValueError(
                f"fragment `{self.name}`'s {port_kind} `{cast}` is wired whole, and is named "
                f"member `{member}`. To fix: wire the member inside the fragment's graph "
                f"generator, at the node whose bundle holds it"
            )
        return cast


@dataclass(frozen=True)
class _RecordedNode:
    name: str
    node_type: str
    input_values: dict[str, Any]
    typed_name: str | None
    class_short_name: str
    fragment: dict[str, Any] | None = field(default=None)

    def as_graph_document_node(self) -> dict[str, Any]:
        node: dict[str, Any] = {
            "name": self.name,
            "type": self.node_type,
            _NODE_INPUT_VALUES_KEY: copy.deepcopy(self.input_values),
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
        self._recorded_groups_by_title: dict[str, dict[str, Any]] = {}
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
        input_values: None = None,
        **promoted_settings: Any,
    ) -> GraphFragmentReference: ...

    @overload
    def add(
        self,
        node_class: FunctionNode[..., Any] | NodeFunction,
        *,
        name: str | None = None,
        input_values: Mapping[str, Any] | None = None,
    ) -> NodeReference: ...

    @overload
    def add(
        self,
        node_class: _NodeClassConstructedWithNothing,
        *,
        name: str | None = None,
        input_values: Mapping[str, Any] | None = None,
    ) -> NodeReference: ...

    @overload
    def add(
        self,
        node_class: type[BuiltInNode],
        *,
        name: str | None = None,
        input_values: InputValuesOfATypedBuiltInNode | None = None,
    ) -> NodeReference: ...

    def add(
        self,
        node_class: object,
        *,
        name: str | None = None,
        input_values: Mapping[str, Any] | None = None,
        **promoted_settings: Any,
    ) -> NodeReference:
        """Record a node of `node_class` — a node class, a function node, or a `@graph_generator` run as a fragment over `promoted_settings` — under `name` (cast) or its short name, cast, holding `input_values`, its value of each input by input name.

        On a typed node each value is checked against the type of the input it names.
        """
        if is_graph_generator(node_class):
            return self._place_the_graph_generator(
                node_class,
                name=name,
                given_input_values=input_values is not None,
                promoted_settings=promoted_settings,
            )
        if "config" in promoted_settings:
            raise TypeError(
                f"`config=` is retired, and {node_class!r} takes its values as "
                f"`input_values={{...}}`, each under the name of the input it sets"
            )
        if promoted_settings:
            raise TypeError(
                f"keyword arguments besides `name` and `input_values` are a graph generator's "
                f"promoted settings, and {node_class!r} is a node, not a "
                f"`@graph_generator`: pass its values as `input_values={{...}}`. Got "
                f"{', '.join(f'`{key}`' for key in promoted_settings)}."
            )
        if not isinstance(node_class, (type, FunctionNode, NodeFunction)):
            raise TypeError(_not_a_node_refusal(node_class))
        node_type = _node_type_of(node_class)
        values = input_values
        typed_pins = TypedNodePins.of(node_class, node_class.__name__)
        node_names_the_values_reference: list[str] = []
        if typed_pins is not None and values is not None:
            given = _input_values_as_a_mapping(values)
            node_names_the_values_reference.extend(
                value.name for value in given.values() if isinstance(value, NodeReference)
            )
            values = typed_pins.stored_input_values(given)
        input_values_as_json_object = _input_values_as_json_object(
            values, node_names_the_values_reference
        )
        for referenced_node_name in node_names_the_values_reference:
            self._refuse_a_node_this_stream_does_not_hold(referenced_node_name)
        node_name = (
            self._name_a_defaulted_node_takes(node_class)
            if name is None
            else self._typed_name_unless_taken(name)
        )
        recorded_node = _RecordedNode(
            name=node_name,
            node_type=node_type,
            input_values=input_values_as_json_object,
            typed_name=name,
            class_short_name=node_class.__name__,
        )
        self._recorded_nodes_by_name[node_name] = recorded_node
        return NodeReference(
            node_name, typed_pins=typed_pins, input_values=input_values_as_json_object
        )

    def _place_the_graph_generator(
        self,
        generator_function: Callable[..., Any],
        *,
        name: str | None,
        given_input_values: bool,
        promoted_settings: dict[str, Any],
    ) -> GraphFragmentReference:
        identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
        if given_input_values:
            raise TypeError(
                f"graph generator `{identity}` takes its promoted settings as keyword "
                f"arguments of `add`, not `input_values`: "
                f"`stream_builder.add({generator_function.__name__}, radius=8)`"
            )
        _refuse_settings_the_generator_does_not_promote(generator_function, promoted_settings)
        node_names_the_settings_reference: list[str] = []
        promoted_settings_as_json = _input_values_as_json_object(
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
        input_port_names = fragment_builder._interface_port_names(_GRAPH_INPUT_NODE_TYPE)
        settings = the_settings_a_graph_generator_promotes(generator_function)
        for setting in settings:
            if setting["name"] in input_port_names:
                raise ValueError(
                    f"graph generator `{identity}` promotes a setting `{setting['name']}` and "
                    f"its fragment declares an Input `{setting['name']}`: the fragment node "
                    f"cannot have two inputs of one name, so rename one"
                )
        if settings:
            fragment["settings"] = settings
        self._recorded_nodes_by_name[node_name] = _RecordedNode(
            name=node_name,
            node_type=identity,
            input_values=promoted_settings_as_json,
            typed_name=name,
            class_short_name=generator_function.__name__,
            fragment=fragment,
        )
        return GraphFragmentReference(
            node_name,
            input_port_names=input_port_names,
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

    def group(
        self,
        title: str,
        nodes: Sequence[NodeReference],
        *,
        description: str = "",
        colour: str = "",
    ) -> None:
        """Record a labelled group of `nodes`, nodes of this fragment, under `title`; it changes nothing that runs, and a refresh keeps its UUID while its title stands."""
        if not self._placed_as_a_fragment:
            raise ValueError(
                f"stream `{self._name}` is no fragment: a generator groups nodes of the "
                f"fragment it builds, and a stream's own groups are made with `add_group`"
            )
        for text, what_it_is in ((title, "title"), (description, "description"), (colour, "colour")):
            if not isinstance(text, str):
                raise TypeError(
                    f"a group's {what_it_is} is a str; got {text!r}, of type "
                    f"`{_type_name_as_written(text)}`"
                )
        if not title:
            raise ValueError(f"a group of fragment `{self._name}` needs a title")
        if title in self._recorded_groups_by_title:
            raise ValueError(
                f"fragment `{self._name}` already holds a group titled {title!r}; a group's "
                f"title names it across every run of the generator, so give it another"
            )
        node_names: list[str] = []
        for node in nodes:
            if not isinstance(node, NodeReference):
                raise TypeError(
                    f"group {title!r} takes the references this fragment's `add` returned; "
                    f"got {node!r}"
                )
            self._refuse_a_node_this_stream_does_not_hold(node.name)
            for held_title, held in self._recorded_groups_by_title.items():
                if node.name in held["nodes"]:
                    raise ValueError(
                        f"node `{node.name}` already belongs to group {held_title!r} of "
                        f"fragment `{self._name}`; a node belongs to one group"
                    )
            if node.name not in node_names:
                node_names.append(node.name)
        recorded_group: dict[str, Any] = {"title": title, "nodes": node_names}
        if description:
            recorded_group["description"] = description
        if colour:
            recorded_group["colour"] = colour
        self._recorded_groups_by_title[title] = recorded_group

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
        self, node_class: type | FunctionNode[..., Any] | NodeFunction | Callable[..., Any]
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
        link_end = {"node": end.node_name, "port": end.port_name}
        if end.member_name is not None:
            link_end["member"] = end.member_name
        return link_end

    def _interface_port_names(self, interface_node_type: str) -> tuple[str, ...]:
        return tuple(
            _cast_name(str(recorded_node.input_values[_GRAPH_INTERFACE_PORT_CONFIG_KEY]), "port name")
            for recorded_node in self._recorded_nodes_by_name.values()
            if recorded_node.node_type == interface_node_type
            and isinstance(recorded_node.input_values.get(_GRAPH_INTERFACE_PORT_CONFIG_KEY), str)
        )

    def _compiled_fragment(self) -> dict[str, Any]:
        groups = (
            {"groups": copy.deepcopy(list(self._recorded_groups_by_title.values()))}
            if self._recorded_groups_by_title
            else {}
        )
        return {
            "nodes": [
                recorded_node.as_graph_document_node()
                for recorded_node in self._recorded_nodes_by_name.values()
            ],
            "links": copy.deepcopy(self._recorded_links),
            **groups,
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


def _cast_name(name: str, what_the_name_names: str) -> str:
    if not isinstance(name, str):
        raise TypeError(
            f"a {what_the_name_names} is a str; got {name!r}, of type "
            f"`{_type_name_as_written(name)}` — pass the name as a str"
        )
    return cast_exposed_name_to_url_safe(name)


def _refuse_a_member_name_that_is_not_a_string(member_name: object) -> None:
    if member_name is not None and not isinstance(member_name, str):
        raise TypeError(
            f"a member name is a str; got {member_name!r}, of type "
            f"`{_type_name_as_written(member_name)}` — pass the member as its bundle names it"
        )


def _not_a_node_refusal(node_class: object) -> str:
    return (
        f"{node_class!r} is not a node: decorate the class or the function with bare "
        f"`@node`, and pass it itself rather than an instance of it or a call of it"
    )


def _node_type_of(node_class: type | FunctionNode[..., Any] | NodeFunction) -> str:
    if hasattr(node_class, NODE_DECLARATION_DECLARED_STAMP):
        return _node_class_import_path(node_class)
    built_in_node_type = getattr(node_class, _BUILT_IN_NODE_TYPE_ATTRIBUTE, None)
    if isinstance(built_in_node_type, str):
        return built_in_node_type
    raise TypeError(_not_a_node_refusal(node_class))


def _node_class_import_path(node_class: type | FunctionNode[..., Any] | NodeFunction) -> str:
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
            f"function's arguments, declare those as its inputs and pass them through "
            f"`stream_builder.add(..., input_values={{...}})` instead — input values reach the "
            f"child, a closure cannot."
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
            f"    @node\n"
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
            f"    @node\n"
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


_INPUT_VALUES_ARGUMENT = "input_values"
"""The argument of `add` a node's values are given as."""


def _input_values_as_a_mapping(values: object) -> Mapping[Any, Any]:
    if not isinstance(values, Mapping):
        raise TypeError(
            f"{_INPUT_VALUES_ARGUMENT} must be a mapping of input names to JSON values — the object a "
            f"node's values are; got {values!r}, of type `{_type_name_as_written(values)}`"
        )
    return values  # pyright: ignore[reportUnknownVariableType]


def _input_values_as_json_object(
    values: Mapping[str, Any] | None,
    node_names_the_values_reference: list[str],
) -> dict[str, Any]:
    if values is None:
        return {}
    as_a_mapping = _input_values_as_a_mapping(values)
    return _json_object(
        as_a_mapping,
        _INPUT_VALUES_ARGUMENT,
        {id(as_a_mapping): _INPUT_VALUES_ARGUMENT},
        node_names_the_values_reference,
    )


def _argument_named_by(key_path: str) -> str:
    """The argument of `add` a key path is inside: its first segment, `input_values` in `input_values['size']`."""
    return key_path.partition("[")[0]


def _json_object(
    mapping: Mapping[Any, Any],
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
    node_names_the_values_reference: list[str],
) -> dict[str, Any]:
    json_object: dict[str, Any] = {}
    for key, value in mapping.items():
        if not isinstance(key, str):
            raise TypeError(
                f"{_argument_named_by(key_path)} must be JSON: `{key_path}` has the key {key!r}, of type "
                f"`{_type_name_as_written(key)}`, and a JSON object's keys are strings — "
                f"convert it with `str(...)`"
            )
        plain_key = str.__str__(key)
        if plain_key in json_object:
            raise ValueError(
                f"{_argument_named_by(key_path)} must be JSON: `{key_path}` has two keys that are both "
                f"{plain_key!r} as plain strings, and a JSON object holds a key once — "
                f"keep one of them, or rename the other"
            )
        json_object[plain_key] = _json_value(
            value,
            f"{key_path}[{plain_key!r}]",
            key_paths_of_enclosing_containers_by_id,
            node_names_the_values_reference,
        )
    return json_object


def _json_value(
    value: object,
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
    node_names_the_values_reference: list[str],
) -> Any:
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, NodeReference):
        _refuse_a_container_nested_too_deep(key_path, key_paths_of_enclosing_containers_by_id)
        node_names_the_values_reference.append(value.name)
        return {"node": value.name}
    if isinstance(value, str):
        return str.__str__(value)
    if isinstance(value, int):
        return _json_integer(value, key_path)
    if isinstance(value, float):
        return _json_float(value, key_path)
    if not isinstance(value, (Mapping, list, tuple)):
        raise TypeError(
            f"{_argument_named_by(key_path)} must be JSON: `{key_path}` is of type `{_type_name_as_written(value)}`, "
            f"and a graph carries only dict, list, tuple, str, int, float, bool, None and the "
            f"`NodeReference` `add` returned — "
            f"convert it with `bool(...)`, `int(...)`, `float(...)` or `str(...)`, or a "
            f"collection with `list(...)`"
        )
    enclosing_key_path = key_paths_of_enclosing_containers_by_id.get(id(value))
    if enclosing_key_path is not None:
        raise ValueError(
            f"{_argument_named_by(key_path)} must be JSON: `{key_path}` is `{enclosing_key_path}`, which holds "
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
            node_names_the_values_reference,
        )
    else:
        json_container = [
            _json_value(
                item,
                f"{key_path}[{index}]",
                key_paths_of_enclosing_containers_by_id,
                node_names_the_values_reference,
            )
            for index, item in enumerate(value)
        ]
    del key_paths_of_enclosing_containers_by_id[id(value)]
    return json_container


def _refuse_a_container_nested_too_deep(
    key_path: str, key_paths_of_enclosing_containers_by_id: dict[int, str]
) -> None:
    containers_enclosing_this_one_counting_the_values = len(
        key_paths_of_enclosing_containers_by_id
    )
    if (
        containers_enclosing_this_one_counting_the_values
        >= _MOST_CONTAINERS_INPUT_VALUES_NEST_COUNTING_THEMSELVES
    ):
        argument = _argument_named_by(key_path)
        raise ValueError(
            f"{argument} nests too deep for a graph: `{key_path}` is a container "
            f"{containers_enclosing_this_one_counting_the_values + 1} deep counting `{argument}` "
            f"itself, and `{argument}` nests at most "
            f"{_MOST_CONTAINERS_INPUT_VALUES_NEST_COUNTING_THEMSELVES} — a graph nests at most "
            f"{_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT} containers from its root, and "
            f"the graph, its `nodes` list and the node enclose every node's values. Nest the data at most "
            f"{_MOST_CONTAINERS_INPUT_VALUES_NEST_COUNTING_THEMSELVES} containers deep, or carry "
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
            f"{_argument_named_by(key_path)} integers must fit the graph's 64-bit range: `{key_path}` is outside "
            f"-2**63 to 2**64 - 1; carry a value that large as a `str`"
        )
    return plain_integer


def _json_float(value: float, key_path: str) -> float:
    plain_float = float.__float__(value)
    if not math.isfinite(plain_float):
        raise ValueError(
            f"{_argument_named_by(key_path)} must be JSON: `{key_path}` is {plain_float!r}, which JSON cannot "
            f"carry — pass `None` where there is no value, or carry it as a `str`"
        )
    return plain_float
