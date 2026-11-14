# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`@node.meta`: what describes a node type, declared beside the decorator that says its kind.

A kind decorator says how a node type is built and runs; `@node.meta` holds what
describes it. It is written above or below the kind decorator. Below, it runs
first and holds what it declares on the function or class until the kind
decorator takes it; above, it stamps a node already declared.
"""

from __future__ import annotations

from typing import Optional

from ._description import refuse_a_description_that_is_not_a_string

__all__ = [
    "NodeMeta",
    "hold_node_meta_for_the_kind_decorator",
    "read_node_meta",
    "take_node_meta_held_for_the_kind_decorator",
]

NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE = "__tatolab_node_meta_held_for_its_kind_decorator__"

# What a category path's levels are separated by, as `Effects/Stylize`.
NODE_CATEGORY_PATH_SEPARATOR = "/"

NodeMeta = dict[str, str]
"""What one `@node.meta` declares, by key: only the keys it was given."""


def read_node_meta(*, category: Optional[str], description: Optional[str]) -> NodeMeta:
    """The keys a `@node.meta(...)` was given, each refused unless it is what its key takes."""
    declared: NodeMeta = {}
    if category is not None:
        declared["category"] = _refuse_a_category_that_is_no_path(category)
    if description is not None:
        declared["description"] = refuse_a_description_that_is_not_a_string(
            description, "@node.meta"
        )
    if not declared:
        raise TypeError(
            "@node.meta() declares nothing; it takes `category`, `description` or both, "
            '`@node.meta(category="Effects/Stylize", description="…")`.'
        )
    return declared


def _refuse_a_category_that_is_no_path(category: object) -> str:
    if not isinstance(category, str):
        raise TypeError(
            f"@node.meta takes its category as a str, got {category!r}. A category is "
            f'the group an editor files the node type under, `"Effects"`, or a path '
            f'into a group inside one, `"Effects/Stylize"`.'
        )
    if any(not level.strip() for level in category.split(NODE_CATEGORY_PATH_SEPARATOR)):
        raise ValueError(
            f"@node.meta category {category!r} names a group with no name. A category is "
            f'`"Effects"` or a path whose every level is named, `"Effects/Stylize"`.'
        )
    return category


def hold_node_meta_for_the_kind_decorator(node_under_declaration: object, declared: NodeMeta) -> None:
    """Keep what a `@node.meta` under a kind decorator declares where that decorator takes it."""
    held = take_node_meta_held_for_the_kind_decorator(node_under_declaration)
    setattr(
        node_under_declaration,
        NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE,
        {**held, **declared},
    )


def take_node_meta_held_for_the_kind_decorator(node_under_declaration: object) -> NodeMeta:
    """What `@node.meta` declared under the kind decorator now declaring this node, taken off it.

    Read from the function's or class's own attributes, so a class never takes what a
    class it extends declared.
    """
    held = vars(node_under_declaration).get(NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE)
    if held is None:
        return {}
    delattr(node_under_declaration, NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE)
    return dict(held)
