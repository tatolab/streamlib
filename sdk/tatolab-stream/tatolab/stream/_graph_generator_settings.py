# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The settings a `@graph_generator` promotes, as the inputs of the fragment node it places.

Pure Python, like the builder that places the fragment.
"""

from __future__ import annotations

import inspect
import typing
from collections.abc import Callable
from typing import Any

from ._description import description_and_annotation_without_it
from ._input_declaration_annotations import HandedToItsNode
from ._node_declaration_refusal import NodeDeclarationRefusal
from ._pin_declaration import NO_DEFAULT, read_pin_declaration

__all__ = ["the_settings_a_graph_generator_promotes"]


def the_settings_a_graph_generator_promotes(
    generator_function: Callable[..., Any],
) -> list[dict[str, Any]]:
    """Each setting `generator_function` promotes, as an input of its fragment node a stream
    reads it by: named as its parameter, typed by its annotation, read at setup, with its
    default. A parameter no pin type types, one with no annotation or one naming no pin type,
    has no input: the generator still runs with the value its fragment node holds for it."""
    generator_name = generator_function.__name__
    try:
        annotations = typing.get_type_hints(generator_function, include_extras=True)
    except Exception:  # noqa: BLE001 — an annotation that does not resolve types no input
        annotations = {}
    parameters = list(inspect.signature(generator_function).parameters.values())[1:]
    settings: list[dict[str, Any]] = []
    for parameter in parameters:
        if parameter.name not in annotations:
            continue
        description, annotation = description_and_annotation_without_it(
            annotations[parameter.name], f"{generator_name}'s setting `{parameter.name}`"
        )
        try:
            declared = read_pin_declaration(
                generator_name,
                parameter.name,
                annotation,
                default=(
                    NO_DEFAULT
                    if parameter.default is inspect.Parameter.empty
                    else parameter.default
                ),
                direction="input",
            )
        except (NodeDeclarationRefusal, TypeError, ValueError):
            continue
        setting: dict[str, Any] = {
            "name": parameter.name,
            "description": description or "",
            "pin_declaration": declared.declaration,
            "input_mode": HandedToItsNode().input_mode(),
        }
        if declared.default_value is not NO_DEFAULT:
            setting["pin_default"] = declared.default_value
        settings.append(setting)
    return settings
