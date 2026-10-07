# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one place `tatolab.stream` reaches the runtime.

`tatolab.runtime` is lent only to the interpreter the runtime starts for a
node, so it is imported here on first use and never statically: a stream
module imports anywhere, and a runtime-backed call outside that interpreter
raises `RuntimeError` naming the function called.
"""

from __future__ import annotations

import functools
import importlib
from collections.abc import Callable
from types import ModuleType
from typing import Any, TypeVar, cast

RUNTIME_PACKAGE_NAME = "tatolab.runtime"
RUNTIME_ENGINE_MODULE_NAME = "tatolab.runtime._engine"

# The `tatolab` namespace always resolves (this package lives in it), so only a
# missing runtime package or engine module means nothing is lent; any other
# import failure is an installed runtime that is broken.
_MODULE_NAMES_WHOSE_ABSENCE_MEANS_NOTHING_IS_LENT = frozenset(
    {RUNTIME_PACKAGE_NAME, RUNTIME_ENGINE_MODULE_NAME}
)

RuntimeBackedFunctionDeclaration = TypeVar(
    "RuntimeBackedFunctionDeclaration", bound=Callable[..., Any]
)

RuntimeBackedProtocolDeclaration = TypeVar(
    "RuntimeBackedProtocolDeclaration", bound="type[object]"
)


runtime_backed_function_registry: dict[Callable[..., Any], str] = {}
"""Every runtime-backed function's forwarder, to the name of the native callable it forwards to."""

runtime_backed_protocol_registry: dict[str, type] = {}
"""Every runtime-backed Protocol, keyed by the name of the native class it declares."""

_native_callables_resolved_by_name: dict[str, Callable[..., Any]] = {}


class RuntimeIsNotLentToThisInterpreterError(RuntimeError):
    """A runtime-backed call made where no runtime lends `tatolab.runtime`."""


def runtime_engine_module_lent_to_this_interpreter(
    called_function_name: str,
) -> ModuleType:
    """Import `tatolab.runtime._engine`, or raise naming the caller where it is absent.

    An engine that is installed but fails to import raises its own error.
    """
    try:
        return importlib.import_module(RUNTIME_ENGINE_MODULE_NAME)
    except ModuleNotFoundError as engine_import_failure:
        if (
            engine_import_failure.name
            not in _MODULE_NAMES_WHOSE_ABSENCE_MEANS_NOTHING_IS_LENT
        ):
            raise
        raise RuntimeIsNotLentToThisInterpreterError(
            f"{called_function_name}() runs only in the interpreter the runtime "
            "starts for a node, where tatolab.runtime is lent; "
            f"{RUNTIME_ENGINE_MODULE_NAME} cannot be imported here "
            f"({engine_import_failure})"
        ) from engine_import_failure


def native_callable_lent_by_the_runtime(
    native_callable_name: str, called_function_name: str
) -> Callable[..., Any]:
    """The runtime's callable named `native_callable_name`, resolved once and cached."""
    native_callable = _native_callables_resolved_by_name.get(native_callable_name)
    if native_callable is None:
        engine_module = runtime_engine_module_lent_to_this_interpreter(
            called_function_name
        )
        native_callable = cast(
            "Callable[..., Any]", getattr(engine_module, native_callable_name)
        )
        _native_callables_resolved_by_name[native_callable_name] = native_callable
    return native_callable


def runtime_backed_function(
    native_callable_name: str | None = None,
) -> Callable[[RuntimeBackedFunctionDeclaration], RuntimeBackedFunctionDeclaration]:
    """Make the decorated def the one declaration of a runtime-backed function.

    The def's signature and docstring are the contract; its body never runs.
    Calls forward to the runtime's native callable of the same name, or of
    `native_callable_name` when given.
    """

    def forward_calls_to_the_native_callable(
        declaration: RuntimeBackedFunctionDeclaration,
    ) -> RuntimeBackedFunctionDeclaration:
        resolved_native_callable_name = native_callable_name or declaration.__name__
        called_function_name = declaration.__name__

        @functools.wraps(declaration)
        def forward_to_native_callable(
            *arguments: Any, **keyword_arguments: Any
        ) -> Any:
            return native_callable_lent_by_the_runtime(
                resolved_native_callable_name, called_function_name
            )(*arguments, **keyword_arguments)

        runtime_backed_function_registry[forward_to_native_callable] = (
            resolved_native_callable_name
        )
        return cast(RuntimeBackedFunctionDeclaration, forward_to_native_callable)

    return forward_calls_to_the_native_callable


def runtime_backed_protocol(
    protocol: RuntimeBackedProtocolDeclaration,
) -> RuntimeBackedProtocolDeclaration:
    """Make the decorated Protocol the one declaration of the runtime's class of the same name."""
    runtime_backed_protocol_registry[protocol.__name__] = protocol
    return protocol


def native_callable_of_runtime_backed_function(
    declared_runtime_backed_function: RuntimeBackedFunctionDeclaration,
    called_function_name: str,
) -> RuntimeBackedFunctionDeclaration:
    """The native callable a runtime-backed function forwards to, typed as its declaration.

    For a caller that reaches it on behalf of a function of its own: where
    nothing is lent, the error names `called_function_name`.
    """
    return cast(
        RuntimeBackedFunctionDeclaration,
        native_callable_lent_by_the_runtime(
            runtime_backed_function_registry[declared_runtime_backed_function],
            called_function_name,
        ),
    )
