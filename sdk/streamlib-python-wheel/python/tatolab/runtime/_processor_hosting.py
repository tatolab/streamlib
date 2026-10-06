# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Turning a declared processor class into a running processor object.

The engine calls into this module rather than constructing the object itself:
building a processor's config class out of the configuration is Python's job.
App code never calls anything here.
"""

from __future__ import annotations

from typing import Any, Optional

__all__ = ["apply_configuration", "construct_processor_instance"]


def construct_processor_instance(
    processor_class: type,
    configuration: Optional[Any],
    _link_data_access: Any,
) -> Any:
    """Instantiate `processor_class` with its configuration.

    The configuration arrives as the mapping the class was added with and is
    constructed into the config class `__init__` names. Construction is the
    only check performed here: how strict it is is the author's choice of
    config class, the same dial `read(port, into=T)` is. The link data access
    argument is unused — ports are reached through `ctx.inputs` / `ctx.outputs`
    — but the host still passes it, so the arity stays.
    """
    config_class = getattr(processor_class, "__streamlib_processor_config_class__", None)
    configuration = _as_configuration_mapping(processor_class, configuration)
    if config_class is None:
        _refuse_a_configuration_with_nowhere_to_go(processor_class, configuration)
        return processor_class()
    return processor_class(config=config_class(**configuration))


def apply_configuration(processor_instance: Any, configuration: Optional[Any]) -> None:
    """Hand a live processor a configuration update.

    Only processors that define `configure` accept one; for anything else a
    config change means a new pipeline, which is what re-running `dev` does.
    """
    processor_class = type(processor_instance)
    reconfigure = getattr(processor_instance, "configure", None)
    if reconfigure is None:
        raise TypeError(
            f"{processor_class.__name__} cannot be reconfigured while running: "
            f"define `configure(self, config)` on it to take one."
        )
    config_class = getattr(processor_class, "__streamlib_processor_config_class__", None)
    configuration = _as_configuration_mapping(processor_class, configuration)
    if config_class is None:
        _refuse_a_configuration_with_nowhere_to_go(processor_class, configuration)
        reconfigure(None)
        return
    reconfigure(config_class(**configuration))


def _refuse_a_configuration_with_nowhere_to_go(
    processor_class: type, configuration: "dict[str, Any]"
) -> None:
    """Refuse a configuration handed to a class that declared none.

    Named rather than discarded, and named the same way the Rust `EmptyConfig`
    names it: a processor that declares no config cannot act on one, and
    silently dropping it hides a wiring mistake.
    """
    if not configuration:
        return
    refused_key = next(iter(configuration))
    raise TypeError(
        f"{processor_class.__name__} declares no config and takes none, so "
        f"`{refused_key}` has nowhere to go. To fix: give its `__init__` a `config` "
        f"parameter annotated with the class those settings live on, or drop the key."
    )


def _as_configuration_mapping(
    processor_class: type, configuration: Optional[Any]
) -> "dict[str, Any]":
    if configuration is None:
        return {}
    if not isinstance(configuration, dict):
        raise TypeError(
            f"config for {processor_class.__name__} must be a dict, got "
            f"{type(configuration).__name__}"
        )
    non_string_keys = [key for key in configuration if not isinstance(key, str)]
    if non_string_keys:
        raise TypeError(
            f"config keys for {processor_class.__name__} must be strings — they name "
            f"the config class's fields; got {non_string_keys!r}"
        )
    return dict(configuration)
