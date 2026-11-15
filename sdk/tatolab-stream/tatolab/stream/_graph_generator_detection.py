# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` mark: stamping a function as a generator, telling one apart, and the generators running now.

Pure Python, like the builder that reads the mark.
"""

from __future__ import annotations

import contextvars
import inspect
from collections.abc import Callable, Iterator, Mapping
from contextlib import contextmanager
from types import FunctionType
from typing import TYPE_CHECKING, Any, TypeGuard, TypeVar


_GraphGeneratorFunction = TypeVar(
    "_GraphGeneratorFunction", bound="Callable[..., object]"
)

_GRAPH_GENERATOR_IDENTITY_ATTRIBUTE = "__streamlib_graph_generator_identity__"
_GRAPH_GENERATOR_NAME_ATTRIBUTE = "__streamlib_graph_generator_name__"
_GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE = "__streamlib_graph_generator_description__"

# CPython puts this in `__qualname__` for anything defined inside a function.
_FUNCTION_LOCAL_MARKER = "<locals>"

_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS = (
    "@graph_generator takes no arguments: the name is the function's. Write "
    "`@graph_generator` bare above `def main(stream_builder: StreamBuilder) -> None:`, "
    "each parameter after the builder a setting the generator promotes."
)

# The keywords `StreamBuilder.add` reads itself, which no generator may promote.
_KEYWORDS_ADD_TAKES_ITSELF = ("name", "config")

# The generators running in this run, outermost first, by name and identity.
_GRAPH_GENERATORS_RUNNING: contextvars.ContextVar[tuple[tuple[str, str], ...]] = (
    contextvars.ContextVar("_GRAPH_GENERATORS_RUNNING", default=())
)


# A type checker sees the one-argument signature and flags `@graph_generator()`
# and `@graph_generator(name=...)` where they are written; the call itself
# accepts them only to refuse them by name.
if TYPE_CHECKING:

    def graph_generator(
        graph_generator_function: _GraphGeneratorFunction, /
    ) -> _GraphGeneratorFunction:
        """Mark a module-level function of a `StreamBuilder` and its promoted settings as a graph generator; return it unchanged."""
        ...

else:

    def graph_generator(graph_generator_function=None, /, **misused_keyword_arguments):
        """Mark a module-level function of a `StreamBuilder` and its promoted settings as a graph generator; return it unchanged."""
        if graph_generator_function is None or misused_keyword_arguments:
            raise TypeError(_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS)
        return _stamp_graph_generator(graph_generator_function)


def _stamp_graph_generator(candidate: object) -> FunctionType:
    generator_function = _the_function_unless_it_cannot_be_a_graph_generator(candidate)
    module = generator_function.__module__
    qualname = generator_function.__qualname__
    setattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE, f"{module}:{qualname}")
    setattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE, generator_function.__name__)
    setattr(
        generator_function,
        _GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE,
        inspect.getdoc(generator_function) or "",
    )
    return generator_function


def _the_function_unless_it_cannot_be_a_graph_generator(candidate: object) -> FunctionType:
    if not inspect.isfunction(candidate):
        # What a class-member decorator returns: a descriptor, callable only for
        # `staticmethod`.
        if isinstance(candidate, staticmethod) or (
            hasattr(type(candidate), "__get__") and not callable(candidate)
        ):
            descriptor_type_name = _type_name_as_written(candidate)
            raise TypeError(
                f"@graph_generator decorates a plain module-level function taking a "
                f"`StreamBuilder`, and {candidate!r} is of type `{descriptor_type_name}`, "
                f"which makes a class member of what it wraps. Put `@graph_generator` on a "
                f"plain module-level `def`, with no `@{descriptor_type_name}` under it."
            )
        # Stacked over a `def` or a class `@graph_generator` receives something
        # callable, and over a class-member decorator the descriptor above, so any
        # other value reached it as an argument: `@graph_generator("rig")`.
        sentence_naming_the_bare_form_when_called_with_a_value = (
            "" if callable(candidate) else f" {_GRAPH_GENERATOR_TAKES_NO_ARGUMENTS}"
        )
        raise TypeError(
            f"@graph_generator decorates a plain module-level function taking a "
            f"`StreamBuilder`, and {candidate!r} is not one."
            f"{sentence_naming_the_bare_form_when_called_with_a_value}"
        )
    generator_function = candidate
    module = generator_function.__module__
    qualname = generator_function.__qualname__
    if generator_function.__name__ == "<lambda>":
        raise TypeError(
            f"a graph generator is a module-level function defined with `def`; "
            f"`{module}:{qualname}` is a lambda, which has no name to load it by"
        )
    if (
        inspect.iscoroutinefunction(generator_function)
        or inspect.isgeneratorfunction(generator_function)
        or inspect.isasyncgenfunction(generator_function)
    ):
        raise TypeError(
            f"`{module}:{qualname}` is an `async def` or generator function, so calling it "
            f"over the builder runs none of its body. A graph generator is a plain `def` "
            f"that adds its nodes when called."
        )
    if _FUNCTION_LOCAL_MARKER in qualname or "." in qualname:
        raise TypeError(
            f"a graph generator is a module-level function, and `{module}:{qualname}` is "
            f"defined inside a "
            f"{'function' if _FUNCTION_LOCAL_MARKER in qualname else 'class'}, so nothing "
            f"can load it by name. Move `{generator_function.__name__}` to module scope."
        )
    parameters = list(inspect.signature(generator_function).parameters.values())
    takes_the_builder_first = bool(parameters) and parameters[0].kind in (
        inspect.Parameter.POSITIONAL_ONLY,
        inspect.Parameter.POSITIONAL_OR_KEYWORD,
    )
    promoted_settings_are_named = all(
        parameter.kind
        in (inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY)
        for parameter in parameters[1:]
    )
    if not (takes_the_builder_first and promoted_settings_are_named):
        raise TypeError(
            f"a graph generator takes the `StreamBuilder` it builds on as its one "
            f"positional parameter, then each setting it promotes by name — "
            f"`def {generator_function.__name__}(stream_builder: StreamBuilder, "
            f"radius: int = 4) -> None:`; `{module}:{qualname}` is "
            f"`{generator_function.__name__}{inspect.signature(generator_function)}`"
        )
    settings_add_takes_itself = [
        parameter.name
        for parameter in parameters[1:]
        if parameter.name in _KEYWORDS_ADD_TAKES_ITSELF
    ]
    if settings_add_takes_itself:
        raise TypeError(
            f"graph generator `{module}:{qualname}` promotes "
            f"{', '.join(f'`{name}`' for name in settings_add_takes_itself)}, which "
            f"`stream_builder.add` takes itself — the fragment's name and a node's config — "
            f"so no placement could set it. Give the setting another name."
        )
    return generator_function


def is_graph_generator(candidate: object) -> TypeGuard[Callable[..., Any]]:
    """Whether `candidate` is a function `@graph_generator` declared."""
    return inspect.isfunction(candidate) and all(
        isinstance(getattr(candidate, stamp, None), str)
        for stamp in (
            _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE,
            _GRAPH_GENERATOR_NAME_ATTRIBUTE,
            _GRAPH_GENERATOR_DESCRIPTION_ATTRIBUTE,
        )
    )


@contextmanager
def _running_the_graph_generator(generator_function: Callable[..., Any]) -> Iterator[None]:
    """Mark `generator_function` running for as long as the block runs, refusing one already running higher in this run."""
    identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
    name: str = getattr(generator_function, _GRAPH_GENERATOR_NAME_ATTRIBUTE)
    running = _GRAPH_GENERATORS_RUNNING.get()
    if any(running_identity == identity for _, running_identity in running):
        chain = " → ".join([*(running_name for running_name, _ in running), name])
        raise ValueError(
            f"graph generator `{identity}` places itself: {chain}. A generator already "
            f"running higher in this run is never placed again, so the graph it builds "
            f"ends; nothing of this run is kept"
        )
    token = _GRAPH_GENERATORS_RUNNING.set((*running, (name, identity)))
    try:
        yield
    finally:
        _GRAPH_GENERATORS_RUNNING.reset(token)


def _refuse_settings_the_generator_does_not_promote(
    generator_function: Callable[..., Any], promoted_settings: Mapping[str, Any]
) -> None:
    identity: str = getattr(generator_function, _GRAPH_GENERATOR_IDENTITY_ATTRIBUTE)
    parameters = list(inspect.signature(generator_function).parameters.values())[1:]
    promoted = {parameter.name: parameter for parameter in parameters}
    unknown = [key for key in promoted_settings if key not in promoted]
    missing = [
        parameter.name
        for parameter in parameters
        if parameter.default is inspect.Parameter.empty
        and parameter.name not in promoted_settings
    ]
    if not (unknown or missing):
        return
    promoted_listed = ", ".join(f"`{name}`" for name in promoted) or "none"
    what_is_wrong = []
    if unknown:
        what_is_wrong.append(
            f"it promotes no {', '.join(f'`{key}`' for key in unknown)}"
        )
    if missing:
        what_is_wrong.append(
            f"{', '.join(f'`{name}`' for name in missing)} has no default and is not given"
        )
    raise TypeError(
        f"graph generator `{identity}`: {'; '.join(what_is_wrong)}. The settings it "
        f"promotes are: {promoted_listed}"
    )


def _type_name_as_written(value: object) -> str:
    value_type = type(value)
    if value_type.__module__ == "builtins":
        return value_type.__qualname__
    return f"{value_type.__module__}.{value_type.__qualname__}"
