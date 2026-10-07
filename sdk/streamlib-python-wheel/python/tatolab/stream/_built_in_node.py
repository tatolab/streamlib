# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The base every generated built-in node class derives from."""

from __future__ import annotations

from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, ClassVar, Generic, NoReturn, TypeVar

__all__ = ["BuiltInNode", "ConfigTypeOfTheBuiltInNode"]

# Contravariant so that pyright, solving a config's type from the class a call
# passes, reports a wrong config key against the config rather than the class.
ConfigTypeOfTheBuiltInNode = TypeVar(
    "ConfigTypeOfTheBuiltInNode", bound=Mapping[str, Any], contravariant=True
)


class BuiltInNode(Generic[ConfigTypeOfTheBuiltInNode]):
    """A node the runtime implements: passed to `stream_builder.add`, never built."""

    type: ClassVar[str]
    """The path the runtime registers this node under: the `type` a graph names."""

    if TYPE_CHECKING:
        # Uncallable, so a built-in never matches `add`'s overload for a node
        # class Python constructs, and a wrong config cannot fall through to it.
        def __init__(self, *, a_built_in_node_is_never_constructed: NoReturn) -> None: ...

    else:

        def __init__(self, *_arguments: object, **_keyword_arguments: object) -> None:
            raise TypeError(
                f"`{type(self).__name__}` is a built-in node, which the runtime runs; it "
                f"is never constructed in Python. Pass the class itself to "
                f"`stream_builder.add({type(self).__name__}, config={{...}})`."
            )
