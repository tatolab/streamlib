# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream: a `@stream` function over a `Stream` builder, compiled to its graph.

Pure Python — the standard library and the name cast, nothing native — so a
stream module imports and compiles with no engine in the process. The graph
`compile_stream_to_graph` returns is the mapping `Runtime.load` takes.
"""

from __future__ import annotations

import copy
import inspect
import math
import sys
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from types import FunctionType
from typing import TYPE_CHECKING, Any, TypeVar

from ._exposed_name_cast import (
    EXPOSED_NAME_MAXIMUM_LENGTH,
    cast_exposed_name_to_url_safe,
)

__all__ = [
    "NodeInputPortReference",
    "NodeOutputPortReference",
    "NodeReference",
    "RemoteNodeInputPortReference",
    "RemoteNodeOutputPortReference",
    "Stream",
    "compile_stream_to_graph",
    "is_stream_function",
    "stream",
]

_StreamFunction = TypeVar("_StreamFunction", bound="Callable[[Stream], object]")

_STREAM_IDENTITY_ATTRIBUTE = "__streamlib_stream_identity__"
_STREAM_NAME_ATTRIBUTE = "__streamlib_stream_name__"
_STREAM_DESCRIPTION_ATTRIBUTE = "__streamlib_stream_description__"

# The stamp `@node` leaves on a class it declares.
_NODE_DECLARED_ATTRIBUTE = "__streamlib_processor_declared__"
# The class attribute a native marker carries its registered import path in.
_NATIVE_MARKER_TYPE_ATTRIBUTE = "type"

# The module name CPython gives the file it was launched with; a child process
# importing this name gets its own entry file instead.
_ENTRY_FILE_MODULE = "__main__"
# CPython puts this in `__qualname__` for anything defined inside a function.
_FUNCTION_LOCAL_MARKER = "<locals>"

# A mesh address is `<runtime name>/<node>/<port>`, each part one Zenoh key
# chunk; the engine's twin is `core/runtime/mesh_address_chunk.rs`.
_CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN = ("/", "*", "$", "#", "?")
_CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH = "@"

_STREAM_TAKES_NO_ARGUMENTS = (
    "@stream takes no arguments: the name is the function's, overridden at load "
    "with `--name`. Write `@stream` bare above `def main(stream: Stream) -> None:`."
)

# A type checker sees the one-argument signature and flags `@stream()` and
# `@stream(name=...)` where they are written; the call itself accepts them only
# to refuse them by name.
if TYPE_CHECKING:

    def stream(stream_function: _StreamFunction, /) -> _StreamFunction:
        """Mark a module-level function taking one `Stream` as a stream, and return it unchanged."""
        ...

else:

    def stream(stream_function=None, /, **misused_keyword_arguments):
        """Mark a module-level function taking one `Stream` as a stream, and return it unchanged."""
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
        # Decorating a `def` or a class always passes something callable, so a
        # value that is not came from a call: `@stream("rig")`.
        called_with_an_argument = (
            "" if callable(candidate) else f" {_STREAM_TAKES_NO_ARGUMENTS}"
        )
        raise TypeError(
            f"@stream decorates a plain module-level function taking one `Stream`, and "
            f"{candidate!r} is not one.{called_with_an_argument}"
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
            f"a stream function takes exactly one positional parameter, the `Stream` it "
            f"builds on — `def {stream_function.__name__}(stream: Stream) -> None:`; "
            f"`{module}:{qualname}` is `{stream_function.__name__}"
            f"{inspect.signature(stream_function)}`"
        )
    return stream_function


def is_stream_function(candidate: object) -> bool:
    """Whether `candidate` is a function `@stream` declared."""
    return inspect.isfunction(candidate) and all(
        isinstance(getattr(candidate, stamp, None), str)
        for stamp in (
            _STREAM_IDENTITY_ATTRIBUTE,
            _STREAM_NAME_ATTRIBUTE,
            _STREAM_DESCRIPTION_ATTRIBUTE,
        )
    )


@dataclass(frozen=True, slots=True)
class NodeOutputPortReference:
    """An output port of a node a `Stream` holds — the producing end of a link; names cast."""

    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class NodeInputPortReference:
    """An input port of a node a `Stream` holds — the consuming end of a link; names cast."""

    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class RemoteNodeOutputPortReference:
    """An output port of a node on another runtime, addressed over the mesh; names cast."""

    runtime_name: str
    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        _refuse_a_runtime_name_the_mesh_cannot_carry(self.runtime_name)
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class RemoteNodeInputPortReference:
    """An input port of a node on another runtime, addressed over the mesh; names cast."""

    runtime_name: str
    node_name: str
    port_name: str

    def __post_init__(self) -> None:
        _refuse_a_runtime_name_the_mesh_cannot_carry(self.runtime_name)
        object.__setattr__(self, "node_name", _cast_name(self.node_name, "node name"))
        object.__setattr__(self, "port_name", _cast_name(self.port_name, "port name"))


@dataclass(frozen=True, slots=True)
class NodeReference:
    """A node `Stream.add` recorded, under the cast name links and exposures name it by."""

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


class Stream:
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

    def add(
        self,
        node_class: type,
        *,
        name: str | None = None,
        config: Mapping[str, Any] | None = None,
    ) -> NodeReference:
        """Record a node of `node_class` under `name` (cast) or its class's short name, cast."""
        if not isinstance(node_class, type):
            raise TypeError(_not_a_node_refusal(node_class))
        _refuse_a_node_this_platform_cannot_run(node_class)
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
        source: NodeOutputPortReference | RemoteNodeOutputPortReference,
        destination: NodeInputPortReference | RemoteNodeInputPortReference,
    ) -> None:
        """Record a link from `source` to `destination`."""
        if not isinstance(
            source, (NodeOutputPortReference, RemoteNodeOutputPortReference)
        ):
            raise TypeError(
                f"connect's source must name an output port: `node.output(port_name)` for "
                f"a node this stream added, or `stream.remote_output(runtime_name, "
                f"node_name, port_name)` for one on another runtime. Got {source!r}."
            )
        if not isinstance(
            destination, (NodeInputPortReference, RemoteNodeInputPortReference)
        ):
            raise TypeError(
                f"connect's destination must name an input port: `node.input(port_name)` "
                f"for a node this stream added, or `stream.remote_input(runtime_name, "
                f"node_name, port_name)` for one on another runtime. Got {destination!r}."
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
                f"`node.output(port_name)`. Got {output!r}."
            )
        self._refuse_a_node_this_stream_does_not_hold(output.node_name)
        exposed_output_port = (output.node_name, output.port_name)
        if exposed_output_port in self._exposed_output_ports:
            raise ValueError(
                f"stream `{self._name}` already exposes port `{output.port_name}` of node "
                f"`{output.node_name}`; an output is exposed once"
            )
        self._exposed_output_ports.append(exposed_output_port)

    def remote_output(
        self, runtime_name: str, node_name: str, port_name: str
    ) -> RemoteNodeOutputPortReference:
        """Name an output port on another runtime, to connect into this stream."""
        return RemoteNodeOutputPortReference(runtime_name, node_name, port_name)

    def remote_input(
        self, runtime_name: str, node_name: str, port_name: str
    ) -> RemoteNodeInputPortReference:
        """Name an input port on another runtime, to connect this stream into."""
        return RemoteNodeInputPortReference(runtime_name, node_name, port_name)

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
        self,
        end: NodeOutputPortReference
        | NodeInputPortReference
        | RemoteNodeOutputPortReference
        | RemoteNodeInputPortReference,
    ) -> dict[str, str]:
        if isinstance(
            end, (RemoteNodeOutputPortReference, RemoteNodeInputPortReference)
        ):
            return {
                "runtime_name": end.runtime_name,
                "node": end.node_name,
                "port": end.port_name,
            }
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
    stream_function: Callable[[Stream], object], *, name: str | None = None
) -> dict[str, Any]:
    """Run a `@stream` function once over a fresh `Stream` and return the graph it built."""
    if not is_stream_function(stream_function):
        raise TypeError(
            f"{stream_function!r} is not a stream: decorate it with @stream — a "
            f"module-level `def` taking one `Stream`"
        )
    stream_name = (
        name if name is not None else getattr(stream_function, _STREAM_NAME_ATTRIBUTE)
    )
    builder = Stream(stream_name)
    stream_function(builder)
    return builder._compiled_graph()


def _cast_name(name: str, what_the_name_names: str) -> str:
    if not isinstance(name, str):
        raise TypeError(
            f"a {what_the_name_names} is a str; got {name!r}, a {type(name).__name__}"
        )
    return cast_exposed_name_to_url_safe(name)


def _not_a_node_refusal(node_class: object) -> str:
    return (
        f"{node_class!r} is not a node: decorate the class with @streamlib.node, and "
        f"pass the class itself rather than an instance of it"
    )


def _refuse_a_node_this_platform_cannot_run(node_class: type) -> None:
    if (
        not sys.platform.startswith("linux")
        and node_class.__module__ == "streamlib"
        and node_class.__qualname__ == "VirtualCameraSink"
    ):
        raise RuntimeError(
            "VirtualCameraSink is Linux-only today; this platform is not supported by "
            "the streamlib wheel yet"
        )


def _node_type_of(node_class: type) -> str:
    if hasattr(node_class, _NODE_DECLARED_ATTRIBUTE):
        return _node_class_import_path(node_class)
    native_marker_type = getattr(node_class, _NATIVE_MARKER_TYPE_ATTRIBUTE, None)
    if isinstance(native_marker_type, str):
        return native_marker_type
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
            f"function's arguments, pass those through `stream.add(..., config={{...}})` "
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


def _refuse_a_runtime_name_the_mesh_cannot_carry(runtime_name: str) -> None:
    if not isinstance(runtime_name, str):
        raise TypeError(
            f"a runtime name is a str; got {runtime_name!r}, a "
            f"{type(runtime_name).__name__}"
        )
    reason = _first_reason_this_is_not_one_mesh_address_chunk(runtime_name)
    if reason is not None:
        listed = ", ".join(
            repr(character)
            for character in _CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN
        )
        raise ValueError(
            f"the runtime name {runtime_name!r} cannot be addressed on the mesh: "
            f"{reason}. A port on another runtime is addressed <runtime name>/<node>/"
            f"<port>, so the runtime name is one key chunk: non-empty, containing none "
            f"of {listed}, and not beginning with "
            f"{_CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH!r}. Pass the name that "
            f"runtime runs under — `streamlib nodes` lists them."
        )


def _first_reason_this_is_not_one_mesh_address_chunk(candidate: str) -> str | None:
    if not candidate:
        return "it is empty"
    for character in candidate:
        if character in _CHARACTERS_NO_MESH_ADDRESS_CHUNK_MAY_CONTAIN:
            return f"it contains {character!r}"
    if candidate.startswith(_CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH):
        return f"it begins with {_CHARACTER_NO_MESH_ADDRESS_CHUNK_MAY_BEGIN_WITH!r}"
    return None


def _config_as_json_object(config: Mapping[str, Any] | None) -> dict[str, Any]:
    if config is None:
        return {}
    if not isinstance(config, Mapping):
        raise TypeError(
            f"config must be a mapping of str keys to JSON values — the object a node's "
            f"config is; got {config!r}, a {type(config).__name__}"
        )
    return _json_object(config, "config")


def _json_object(mapping: Mapping[Any, Any], key_path: str) -> dict[str, Any]:
    json_object: dict[str, Any] = {}
    for key, value in mapping.items():
        if not isinstance(key, str):
            raise TypeError(
                f"config must be JSON: `{key_path}` has the key {key!r}, a "
                f"{type(key).__name__}, and a JSON object's keys are strings"
            )
        json_object[key] = _json_value(value, f"{key_path}[{key!r}]")
    return json_object


def _json_value(value: object, key_path: str) -> Any:
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise ValueError(
                f"config must be JSON: `{key_path}` is {value!r}, which JSON cannot carry"
            )
        return value
    if isinstance(value, Mapping):
        return _json_object(value, key_path)
    if isinstance(value, (list, tuple)):
        return [
            _json_value(item, f"{key_path}[{index}]")
            for index, item in enumerate(value)
        ]
    raise TypeError(
        f"config must be JSON: `{key_path}` is a {type(value).__name__}, and a graph "
        f"carries only dict, list, tuple, str, int, float, bool and None"
    )
