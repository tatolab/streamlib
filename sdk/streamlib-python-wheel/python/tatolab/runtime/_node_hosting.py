# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Turning a declared node class into a running node object.

The engine calls into this module rather than constructing the object itself:
building a node's config class out of the configuration is Python's job.
App code never calls anything here.
"""

from __future__ import annotations

from typing import Any, Optional

from tatolab.stream._node_config_schema import (
    with_each_node_reference_field_as_a_node_reference,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_CONFIG_CLASS_STAMP

__all__ = ["apply_configuration", "construct_node_instance"]


def construct_node_instance(
    node_class: type,
    configuration: Optional[Any],
    _link_data_access: Any,
) -> Any:
    """Instantiate `node_class` with its configuration.

    The configuration arrives as the mapping the class was added with and is
    constructed into the config class `__init__` names. Construction is the
    only check performed here: how strict it is is the author's choice of
    config class, the same dial `read(port, into=T)` is. The link data access
    argument is unused — ports are reached through `ctx.inputs` / `ctx.outputs`
    — but the host still passes it, so the arity stays.
    """
    config_class = getattr(node_class, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    configuration = _as_configuration_mapping(node_class, configuration)
    if config_class is None:
        _refuse_a_configuration_with_nowhere_to_go(node_class, configuration)
        return node_class()
    return node_class(
        config=config_class(
            **with_each_node_reference_field_as_a_node_reference(config_class, configuration)
        )
    )


def apply_configuration(node_instance: Any, configuration: Optional[Any]) -> None:
    """Hand a live node a configuration update.

    Only nodes that define `configure` accept one; for anything else a
    config change means a new pipeline, which is what re-running `dev` does.
    """
    node_class = type(node_instance)
    reconfigure = getattr(node_instance, "configure", None)
    if reconfigure is None:
        raise TypeError(
            f"{node_class.__name__} cannot be reconfigured while running: "
            f"define `configure(self, config)` on it to take one."
        )
    config_class = getattr(node_class, NODE_DECLARATION_CONFIG_CLASS_STAMP, None)
    configuration = _as_configuration_mapping(node_class, configuration)
    if config_class is None:
        _refuse_a_configuration_with_nowhere_to_go(node_class, configuration)
        reconfigure(None)
        return
    reconfigure(
        config_class(
            **with_each_node_reference_field_as_a_node_reference(config_class, configuration)
        )
    )


def _refuse_a_configuration_with_nowhere_to_go(
    node_class: type, configuration: "dict[str, Any]"
) -> None:
    """Refuse a configuration handed to a class that declared none.

    Named rather than discarded, and named the same way the Rust `EmptyConfig`
    names it: a node that declares no config cannot act on one, and
    silently dropping it hides a wiring mistake.
    """
    if not configuration:
        return
    refused_key = next(iter(configuration))
    raise TypeError(
        f"{node_class.__name__} declares no config and takes none, so "
        f"`{refused_key}` has nowhere to go. To fix: give its `__init__` a `config` "
        f"parameter annotated with the class those settings live on, or drop the key."
    )


def _as_configuration_mapping(
    node_class: type, configuration: Optional[Any]
) -> "dict[str, Any]":
    if configuration is None:
        return {}
    if not isinstance(configuration, dict):
        raise TypeError(
            f"config for {node_class.__name__} must be a dict, got "
            f"{type(configuration).__name__}"
        )
    non_string_keys = [key for key in configuration if not isinstance(key, str)]
    if non_string_keys:
        raise TypeError(
            f"config keys for {node_class.__name__} must be strings — they name "
            f"the config class's fields; got {non_string_keys!r}"
        )
    return dict(configuration)
