# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Turning a declared processor class into a running processor object.

The engine calls into this module rather than constructing the object itself:
mapping configuration onto constructor keywords is Python's job. App code
never calls anything here.
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

    Configuration arrives as the keyword arguments the class was added with, so
    a processor's settings are ordinary constructor parameters with ordinary
    Python defaults — there is no configuration object to learn. The link data
    access argument is unused: ports are reached through `ctx.inputs` /
    `ctx.outputs`, but the host still passes it, so the arity stays.
    """
    keyword_arguments = _as_keyword_arguments(processor_class, configuration)
    try:
        return processor_class(**keyword_arguments)
    except TypeError as construction_failure:
        raise TypeError(
            f"{processor_class.__name__}({_render_call(keyword_arguments)}) failed: "
            f"{construction_failure}. `rt.add(cls, config={{...}})` passes config as "
            f"keyword arguments to the class."
        ) from construction_failure


def apply_configuration(processor_instance: Any, configuration: Optional[Any]) -> None:
    """Hand a live processor a configuration update.

    Only processors that define `configure` accept one; for anything else a
    config change means a new pipeline, which is what re-running `dev` does.
    """
    reconfigure = getattr(processor_instance, "configure", None)
    if reconfigure is None:
        raise TypeError(
            f"{type(processor_instance).__name__} cannot be reconfigured while running: "
            f"define `configure(self, **config)` on it to accept updates."
        )
    reconfigure(**_as_keyword_arguments(type(processor_instance), configuration))


def _as_keyword_arguments(
    processor_class: type, configuration: Optional[Any]
) -> "dict[str, Any]":
    if configuration is None:
        return {}
    if not isinstance(configuration, dict):
        raise TypeError(
            f"config for {processor_class.__name__} must be a dict of keyword arguments, "
            f"got {type(configuration).__name__}"
        )
    non_string_keys = [key for key in configuration if not isinstance(key, str)]
    if non_string_keys:
        raise TypeError(
            f"config keys for {processor_class.__name__} must be strings — they become "
            f"keyword arguments; got {non_string_keys!r}"
        )
    return dict(configuration)


def _render_call(keyword_arguments: "dict[str, Any]") -> str:
    return ", ".join(f"{name}={value!r}" for name, value in keyword_arguments.items())
