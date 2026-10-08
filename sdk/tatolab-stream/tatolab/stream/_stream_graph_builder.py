# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream: a `@stream` function over a `StreamBuilder`, compiled to its graph.

Pure Python — the standard library, the name cast and the built-in node base,
nothing native — so a
stream module imports and compiles with no engine in the process. The graph
`compile_stream_to_graph` returns is the one `tatolabd` loads, handed over as JSON.
"""

from __future__ import annotations

import copy
import inspect
import math
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from types import FunctionType
from typing import TYPE_CHECKING, Any, Protocol, TypeGuard, TypeVar, overload

from ._built_in_node import BuiltInNode, ConfigTypeOfTheBuiltInNode
from ._exposed_name_cast import (
    EXPOSED_NAME_MAXIMUM_LENGTH,
    cast_exposed_name_to_url_safe,
)
from ._node_declaration import NODE_DECLARATION_DECLARED_STAMP

__all__ = [
    "NodeInputPortReference",
    "NodeOutputPortReference",
    "NodeReference",
    "StreamBuilder",
    "compile_stream_to_graph",
    "is_stream_function",
    "stream",
]

_StreamFunction = TypeVar("_StreamFunction", bound="Callable[[StreamBuilder], object]")

_STREAM_IDENTITY_ATTRIBUTE = "__streamlib_stream_identity__"
_STREAM_NAME_ATTRIBUTE = "__streamlib_stream_name__"
_STREAM_DESCRIPTION_ATTRIBUTE = "__streamlib_stream_description__"

# The class attribute a built-in node carries the path it is registered under in.
_BUILT_IN_NODE_TYPE_ATTRIBUTE = "type"

# The module name CPython gives the file it was launched with; a child process
# importing this name gets its own entry file instead.
_ENTRY_FILE_MODULE = "__main__"
# CPython puts this in `__qualname__` for anything defined inside a function.
_FUNCTION_LOCAL_MARKER = "<locals>"

# The engine reads a config integer as an i64, else a u64
# (`python_bag_conversion.rs`); a graph carries no wider one.
_SMALLEST_INTEGER_A_GRAPH_CARRIES = -(2**63)
_LARGEST_INTEGER_A_GRAPH_CARRIES = 2**64 - 1

# A graph `tatolabd` loads nests at most 128 containers deep from its root, and
# three of them — the graph dict, its `nodes` list and the node dict — enclose
# every config.
_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF = 128 - 3

_STREAM_TAKES_NO_ARGUMENTS = (
    "@stream takes no arguments: the name is the function's, overridden at load "
    "with `--name`. Write `@stream` bare above `def main(stream_builder: StreamBuilder) -> None:`."
)

# A type checker sees the one-argument signature and flags `@stream()` and
# `@stream(name=...)` where they are written; the call itself accepts them only
# to refuse them by name.
if TYPE_CHECKING:

    def stream(stream_function: _StreamFunction, /) -> _StreamFunction:
        """Mark a module-level function of one `StreamBuilder` as a stream; return it unchanged."""
        ...

else:

    def stream(stream_function=None, /, **misused_keyword_arguments):
        """Mark a module-level function of one `StreamBuilder` as a stream; return it unchanged."""
        if stream_function is None or misused_keyword_arguments:
            raise TypeError(_STREAM_TAKES_NO_ARGUMENTS)
        return _stamp_stream_function(stream_function)


def _stamp_stream_function(candidate: object) -> FunctionType:
    stream_function = _the_function_unless_it_cannot_be_a_stream(candidate)
    module = stream_function.__module__
    qualname = stream_function.__qualname__
    setattr(stream_function, _STREAM_IDENTITY_ATTRIBUTE, f"{module}:{qualname}")
    setattr(stream_function, _STREAM_NAME_ATTRIBUTE, stream_function.__name__)
    setattr(
        stream_function,
        _STREAM_DESCRIPTION_ATTRIBUTE,
        inspect.getdoc(stream_function) or "",
    )
    return stream_function


def _the_function_unless_it_cannot_be_a_stream(candidate: object) -> FunctionType:
    if not inspect.isfunction(candidate):
        # What a class-member decorator returns: a descriptor, callable only for
        # `staticmethod`.
        if isinstance(candidate, staticmethod) or (
            hasattr(type(candidate), "__get__") and not callable(candidate)
        ):
            descriptor_type_name = _type_name_as_written(candidate)
            raise TypeError(
                f"@stream decorates a plain module-level function taking one `StreamBuilder`, "
                f"and {candidate!r} is of type `{descriptor_type_name}`, which makes a class "
                f"member of what it wraps. Put `@stream` on a plain module-level `def`, "
                f"with no `@{descriptor_type_name}` under it."
            )
        # Stacked over a `def` or a class `@stream` receives something callable,
        # and over a class-member decorator the descriptor above, so any other
        # value reached it as an argument: `@stream("rig")`.
        sentence_naming_the_bare_form_when_called_with_a_value = (
            "" if callable(candidate) else f" {_STREAM_TAKES_NO_ARGUMENTS}"
        )
        raise TypeError(
            f"@stream decorates a plain module-level function taking one `StreamBuilder`, and "
            f"{candidate!r} is not one.{sentence_naming_the_bare_form_when_called_with_a_value}"
        )
    stream_function = candidate
    module = stream_function.__module__
    qualname = stream_function.__qualname__
    if stream_function.__name__ == "<lambda>":
        raise TypeError(
            f"a stream is a module-level function defined with `def`; `{module}:{qualname}` "
            f"is a lambda, which has no name to load it by"
        )
    if (
        inspect.iscoroutinefunction(stream_function)
        or inspect.isgeneratorfunction(stream_function)
        or inspect.isasyncgenfunction(stream_function)
    ):
        raise TypeError(
            f"`{module}:{qualname}` is an `async def` or generator function, so calling it "
            f"over the builder runs none of its body. A stream is a plain `def` that adds "
            f"its nodes when called."
        )
    if _FUNCTION_LOCAL_MARKER in qualname or "." in qualname:
        raise TypeError(
            f"a stream is a module-level function, and `{module}:{qualname}` is defined "
            f"inside a {'function' if _FUNCTION_LOCAL_MARKER in qualname else 'class'}, "
            f"so nothing can load it by name. Move `{stream_function.__name__}` to module "
            f"scope."
        )
    parameters = list(inspect.signature(stream_function).parameters.values())
    if len(parameters) != 1 or parameters[0].kind not in (
        inspect.Parameter.POSITIONAL_ONLY,
        inspect.Parameter.POSITIONAL_OR_KEYWORD,
    ):
        raise TypeError(
            f"a stream function takes exactly one positional parameter, the `StreamBuilder` "
            f"it builds on — `def {stream_function.__name__}(stream_builder: StreamBuilder) "
            f"-> None:`; "
            f"`{module}:{qualname}` is `{stream_function.__name__}"
            f"{inspect.signature(stream_function)}`"
        )
    return stream_function


def is_stream_function(candidate: object) -> TypeGuard[Callable[..., Any]]:
    """Whether `candidate` is a function `@stream` declared."""
    return inspect.isfunction(candidate) and all(
        isinstance(getattr(candidate, stamp, None), str)
        for stamp in (
            _STREAM_IDENTITY_ATTRIBUTE,
            _STREAM_NAME_ATTRIBUTE,
            _STREAM_DESCRIPTION_ATTRIBUTE,
        )
    )


class _NodeClassConstructedWithoutConfig(Protocol):
    """A `@node` class its interpreter constructs as `cls()`."""

    def __call__(self) -> object: ...


class _NodeClassConstructedWithConfig(Protocol):
    """A `@node` class its interpreter constructs as `cls(config=...)`."""

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
    """A node `StreamBuilder.add` recorded, under the cast name links and exposures name it by."""

    name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "name", _cast_name(self.name, "node name"))

    def output(self, port_name: str) -> NodeOutputPortReference:
        """Name one of this node's output ports, cast as `@node` declared it."""
        return NodeOutputPortReference(self.name, port_name)

    def input(self, port_name: str) -> NodeInputPortReference:
        """Name one of this node's input ports, cast as `@node` declared it."""
        return NodeInputPortReference(self.name, port_name)


@dataclass(frozen=True)
class _RecordedNode:
    name: str
    node_type: str
    config: dict[str, Any]
    typed_name: str | None
    class_short_name: str

    def how_its_name_was_given(self) -> str:
        if self.typed_name is not None:
            return f"typed as {self.typed_name!r}"
        return f"the default name of {self.class_short_name}"


class StreamBuilder:
    """The builder a `@stream` function adds, links and exposes nodes on; it runs nothing."""

    def __init__(self, name: str) -> None:
        self._name = _cast_name(name, "stream name")
        self._recorded_nodes_by_name: dict[str, _RecordedNode] = {}
        self._recorded_links: list[dict[str, dict[str, str]]] = []
        self._exposed_output_ports: list[tuple[str, str]] = []

    @property
    def name(self) -> str:
        """The stream's name, cast."""
        return self._name

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
    ) -> NodeReference:
        """Record a node of `node_class` under `name` (cast) or its class's short name, cast."""
        if not isinstance(node_class, type):
            raise TypeError(_not_a_node_refusal(node_class))
        node_type = _node_type_of(node_class)
        node_name = (
            self._name_a_defaulted_node_takes(node_class)
            if name is None
            else self._typed_name_unless_taken(name)
        )
        recorded_node = _RecordedNode(
            name=node_name,
            node_type=node_type,
            config=_config_as_json_object(config),
            typed_name=name,
            class_short_name=node_class.__name__,
        )
        self._recorded_nodes_by_name[node_name] = recorded_node
        return NodeReference(node_name)

    def connect(
        self,
        source: NodeOutputPortReference,
        destination: NodeInputPortReference,
    ) -> None:
        """Record a link from `source` to `destination`."""
        if not isinstance(source, NodeOutputPortReference):
            raise TypeError(
                f"connect's source must name an output port of a node this stream "
                f"added: `stream_builder.add(...).output(port_name)`. Got {source!r}."
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

    def expose(self, output: NodeOutputPortReference) -> None:
        """Record `output` as one the stream offers beyond this machine."""
        if not isinstance(output, NodeOutputPortReference):
            raise TypeError(
                f"expose takes an output port of a node this stream added — "
                f"`stream_builder.add(...).output(port_name)`. Got {output!r}."
            )
        self._refuse_a_node_this_stream_does_not_hold(output.node_name)
        exposed_output_port = (output.node_name, output.port_name)
        if exposed_output_port in self._exposed_output_ports:
            raise ValueError(
                f"stream `{self._name}` already exposes port `{output.port_name}` of node "
                f"`{output.node_name}`; an output is exposed once"
            )
        self._exposed_output_ports.append(exposed_output_port)

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

    def _name_a_defaulted_node_takes(self, node_class: type) -> str:
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

    def _compiled_graph(self) -> dict[str, Any]:
        return {
            "stream": self._name,
            "nodes": [
                {
                    "name": recorded_node.name,
                    "type": recorded_node.node_type,
                    "config": copy.deepcopy(recorded_node.config),
                }
                for recorded_node in self._recorded_nodes_by_name.values()
            ],
            "links": copy.deepcopy(self._recorded_links),
            "exposed": [
                {"node": node_name, "port": port_name}
                for node_name, port_name in self._exposed_output_ports
            ],
        }


def compile_stream_to_graph(
    stream_function: Callable[[StreamBuilder], object], *, name: str | None = None
) -> dict[str, Any]:
    """Run a `@stream` function once over a fresh `StreamBuilder` and return the graph it built."""
    if not is_stream_function(stream_function):
        raise TypeError(
            f"{stream_function!r} is not a stream: decorate it with @stream — a "
            f"module-level `def` taking one `StreamBuilder`"
        )
    stream_name = (
        name if name is not None else getattr(stream_function, _STREAM_NAME_ATTRIBUTE)
    )
    stream_builder = StreamBuilder(stream_name)
    stream_function(stream_builder)
    return stream_builder._compiled_graph()


def _cast_name(name: str, what_the_name_names: str) -> str:
    if not isinstance(name, str):
        raise TypeError(
            f"a {what_the_name_names} is a str; got {name!r}, of type "
            f"`{_type_name_as_written(name)}` — pass the name as a str"
        )
    return cast_exposed_name_to_url_safe(name)


def _not_a_node_refusal(node_class: object) -> str:
    return (
        f"{node_class!r} is not a node: decorate the class with @tatolab.stream.node, and "
        f"pass the class itself rather than an instance of it"
    )


def _node_type_of(node_class: type) -> str:
    if hasattr(node_class, NODE_DECLARATION_DECLARED_STAMP):
        return _node_class_import_path(node_class)
    built_in_node_type = getattr(node_class, _BUILT_IN_NODE_TYPE_ATTRIBUTE, None)
    if isinstance(built_in_node_type, str):
        return built_in_node_type
    raise TypeError(_not_a_node_refusal(node_class))


def _node_class_import_path(node_class: type) -> str:
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
            f"    @node(...)\n"
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


def _config_as_json_object(config: Mapping[str, Any] | None) -> dict[str, Any]:
    if config is None:
        return {}
    if not isinstance(config, Mapping):
        raise TypeError(
            f"config must be a mapping of str keys to JSON values — the object a node's "
            f"config is; got {config!r}, of type `{_type_name_as_written(config)}`"
        )
    return _json_object(config, "config", {id(config): "config"})


def _json_object(
    mapping: Mapping[Any, Any],
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
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
            value, f"{key_path}[{plain_key!r}]", key_paths_of_enclosing_containers_by_id
        )
    return json_object


def _json_value(
    value: object,
    key_path: str,
    key_paths_of_enclosing_containers_by_id: dict[int, str],
) -> Any:
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, str):
        return str.__str__(value)
    if isinstance(value, int):
        return _json_integer(value, key_path)
    if isinstance(value, float):
        return _json_float(value, key_path)
    if not isinstance(value, (Mapping, list, tuple)):
        raise TypeError(
            f"config must be JSON: `{key_path}` is of type `{_type_name_as_written(value)}`, "
            f"and a graph carries only dict, list, tuple, str, int, float, bool and None — "
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
            f"{_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF} — `tatolabd` counts "
            f"containers from the graph's root, and the graph, its `nodes` list and the "
            f"node enclose every config. Nest the data at most "
            f"{_MOST_CONTAINERS_A_CONFIG_NESTS_COUNTING_ITSELF} containers deep, or carry "
            f"the deeper part as a `str`"
        )
    key_paths_of_enclosing_containers_by_id[id(value)] = key_path
    if isinstance(value, Mapping):
        json_container: Any = _json_object(
            value, key_path, key_paths_of_enclosing_containers_by_id
        )
    else:
        json_container = [
            _json_value(
                item, f"{key_path}[{index}]", key_paths_of_enclosing_containers_by_id
            )
            for index, item in enumerate(value)
        ]
    del key_paths_of_enclosing_containers_by_id[id(value)]
    return json_container


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
