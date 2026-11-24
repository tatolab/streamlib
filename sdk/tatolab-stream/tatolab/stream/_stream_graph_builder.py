# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A graph generator: a `@graph_generator` function over a `StreamBuilder`, run to the graph it builds.

Pure Python — the standard library, the name cast and the built-in node base,
nothing native — so a generator's module imports and runs with no engine in the
process. The graph `run_graph_generator` returns is the one `tatolabd` loads,
handed over as JSON; a generator another one places is stored inside it as a
fragment.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from typing import Any

from ._graph_generator_detection import (
    _GRAPH_GENERATOR_NAME_ATTRIBUTE,
    _refuse_settings_the_generator_does_not_promote,
    _running_the_graph_generator,
    graph_generator,
    is_graph_generator,
)
from ._stream_builder import (
    _MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT,
    GraphFragmentReference,
    NodeInputPortReference,
    NodeOutputPortReference,
    NodeReference,
    StreamBuilder,
)


__all__ = [
    "GraphFragmentReference",
    "NodeInputPortReference",
    "NodeOutputPortReference",
    "NodeReference",
    "StreamBuilder",
    "graph_generator",
    "is_graph_generator",
    "place_graph_generator",
    "run_graph_generator",
]


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
    """Run a `@graph_generator` function as a fragment placed under `name` (cast) or its own name, and return the fragment node a graph document stores: its `name`, its generator as `type`, its promoted settings as `input_values` and its `fragment`."""
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
