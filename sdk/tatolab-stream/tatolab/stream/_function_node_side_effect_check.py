# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The side-effect check: what makes a function node compute from more than its inputs.

Reads the body's source at decoration, never runs it. It names a call that reads
the clock or a random source, opens a file or a socket, or starts a thread, a
`global` or `nonlocal` statement, and a module-level mutable value the body
reads, each with its line and the fix. Access it cannot see in the source, an
alias built at run time or a call made by a function the body calls, is out of
its reach by design.
"""

from __future__ import annotations

import ast
import builtins
import collections.abc
import inspect
import textwrap
import types
from typing import Any, Callable, NamedTuple, Optional

__all__ = [
    "FunctionNodeSideEffectFinding",
    "find_side_effects_in_a_function_node_body",
    "refuse_a_function_node_body_with_side_effects",
]

_TAKE_NOW = "take `now: Time` and compute from it"
_KEEP_IT_IN_STATE = "keep what carries between calls in `State[...]`"
_STEP_A_SEED_IN_STATE = "keep a seed in `State[...]` and step it in the body, or take the value as an input"
_MAKE_IT_A_PROCESS = "make it a `@node.process` class, which may do I/O"
_PASS_IT_IN = "pass it in as `config` or an input, or make it immutable (a tuple, or an array with `setflags(write=False)`)"

# Dotted name a call resolves to -> (what calling it does, the fix). An entry
# also matches every name under it.
SIDE_EFFECTING_CALLABLES: "dict[str, tuple[str, str]]" = {
    "time": ("reads the clock", _TAKE_NOW),
    "datetime.datetime.now": ("reads the clock", _TAKE_NOW),
    "datetime.datetime.utcnow": ("reads the clock", _TAKE_NOW),
    "datetime.datetime.today": ("reads the clock", _TAKE_NOW),
    "datetime.date.today": ("reads the clock", _TAKE_NOW),
    "random": ("draws from a random source", _STEP_A_SEED_IN_STATE),
    "numpy.random": ("draws from a random source", _STEP_A_SEED_IN_STATE),
    "secrets": ("draws from a random source", _STEP_A_SEED_IN_STATE),
    "os.urandom": ("draws from a random source", _STEP_A_SEED_IN_STATE),
    "io.open": ("opens a file", _MAKE_IT_A_PROCESS),
    "os.open": ("opens a file", _MAKE_IT_A_PROCESS),
    "socket": ("opens a socket", _MAKE_IT_A_PROCESS),
    "threading": ("starts or coordinates a thread", _MAKE_IT_A_PROCESS),
}

# The module a C implementation reports -> the module it is reached through.
_PUBLIC_MODULE_OF_AN_IMPLEMENTATION_MODULE = {"_io": "io", "posix": "os", "nt": "os"}


class FunctionNodeSideEffectFinding(NamedTuple):
    """One way a function node's body computes from more than its inputs."""

    line: int
    what_it_does: str
    fix: str


def find_side_effects_in_a_function_node_body(
    function: "Callable[..., Any]",
) -> "list[FunctionNodeSideEffectFinding]":
    """Every side effect `function`'s source shows, in line order; none when its source is unavailable."""
    try:
        source_lines, first_line = inspect.getsourcelines(function)
    except (OSError, TypeError):
        return []
    parsed_source = ast.parse(textwrap.dedent("".join(source_lines)))
    function_definition = parsed_source.body[0]
    if not isinstance(function_definition, (ast.FunctionDef, ast.AsyncFunctionDef)):
        return []
    names_local_to_the_body = frozenset(function.__code__.co_varnames) | frozenset(
        function.__code__.co_cellvars
    )
    module_globals: "dict[str, Any]" = function.__globals__
    findings: "list[FunctionNodeSideEffectFinding]" = []

    def line_of(syntax_node: ast.AST) -> int:
        return first_line + getattr(syntax_node, "lineno", 1) - 1

    body_syntax_nodes = (
        syntax_node
        for statement in function_definition.body
        for syntax_node in ast.walk(statement)
    )
    for syntax_node in body_syntax_nodes:
        if isinstance(syntax_node, ast.Global):
            findings.append(
                FunctionNodeSideEffectFinding(
                    line_of(syntax_node),
                    f"declares `global {', '.join(syntax_node.names)}`, so it writes module state",
                    _KEEP_IT_IN_STATE,
                )
            )
        elif isinstance(syntax_node, ast.Nonlocal):
            findings.append(
                FunctionNodeSideEffectFinding(
                    line_of(syntax_node),
                    f"declares `nonlocal {', '.join(syntax_node.names)}`, so it writes an enclosing function's state",
                    _KEEP_IT_IN_STATE,
                )
            )
        elif isinstance(syntax_node, ast.Call):
            spelled = ast.unparse(syntax_node.func)
            resolved_name = _dotted_name_a_call_resolves_to(
                syntax_node.func, names_local_to_the_body, module_globals
            )
            side_effect = _side_effect_of_calling(resolved_name)
            if side_effect is not None:
                what_calling_it_does, fix = side_effect
                resolved = "" if resolved_name == spelled else f" (`{resolved_name}`)"
                findings.append(
                    FunctionNodeSideEffectFinding(
                        line_of(syntax_node),
                        f"calls `{spelled}`{resolved}, which {what_calling_it_does}",
                        fix,
                    )
                )
        elif (
            isinstance(syntax_node, ast.Name)
            and isinstance(syntax_node.ctx, ast.Load)
            and syntax_node.id not in names_local_to_the_body
            and syntax_node.id in module_globals
            and _is_a_mutable_value(module_globals[syntax_node.id])
        ):
            findings.append(
                FunctionNodeSideEffectFinding(
                    line_of(syntax_node),
                    f"reads the module-level `{syntax_node.id}`, a mutable "
                    f"`{type(module_globals[syntax_node.id]).__name__}` its reader's copy never sees changed",
                    _PASS_IT_IN,
                )
            )
    return sorted(set(findings))


def refuse_a_function_node_body_with_side_effects(function: "Callable[..., Any]") -> None:
    """Refuse `function`, naming each side effect its body shows and the fix."""
    findings = find_side_effects_in_a_function_node_body(function)
    if not findings:
        return
    finding_lines = "\n".join(
        f"  line {finding.line}: {finding.what_it_does}. To fix: {finding.fix}."
        for finding in findings
    )
    raise TypeError(
        f"{function.__name__} must compute only from its inputs, `config`, `now` and its own "
        f"`State`, and its body:\n{finding_lines}"
    )


def _dotted_name_a_call_resolves_to(
    called: ast.expr, names_local_to_the_body: "frozenset[str]", module_globals: "dict[str, Any]"
) -> str:
    attribute_names: "list[str]" = []
    while isinstance(called, ast.Attribute):
        attribute_names.insert(0, called.attr)
        called = called.value
    if not isinstance(called, ast.Name) or called.id in names_local_to_the_body:
        return ""
    if called.id in module_globals:
        root_value = module_globals[called.id]
    elif hasattr(builtins, called.id):
        root_value = getattr(builtins, called.id)
    else:
        return ""
    root_name = _dotted_name_of(root_value)
    if root_name is None:
        return ""
    return ".".join([root_name, *attribute_names])


def _dotted_name_of(value: Any) -> Optional[str]:
    if isinstance(value, types.ModuleType):
        return value.__name__
    bound_to = getattr(value, "__self__", None)
    if bound_to is not None and not isinstance(bound_to, (type, types.ModuleType)):
        module_name = type(bound_to).__module__
        name = getattr(value, "__name__", None)
        return None if name is None else f"{_public_module_of(module_name)}.{name}"
    module_name = getattr(value, "__module__", None)
    qualified_name = getattr(value, "__qualname__", None)
    if not isinstance(module_name, str) or not isinstance(qualified_name, str):
        return None
    return f"{_public_module_of(module_name)}.{qualified_name}"


def _public_module_of(module_name: str) -> str:
    return _PUBLIC_MODULE_OF_AN_IMPLEMENTATION_MODULE.get(module_name, module_name)


def _side_effect_of_calling(resolved_name: str) -> "Optional[tuple[str, str]]":
    if not resolved_name:
        return None
    for refused_name, side_effect in SIDE_EFFECTING_CALLABLES.items():
        if resolved_name == refused_name or resolved_name.startswith(refused_name + "."):
            return side_effect
    return None


def _is_a_mutable_value(value: Any) -> bool:
    if isinstance(value, (types.ModuleType, type, types.FunctionType, types.BuiltinFunctionType)):
        return False
    if isinstance(
        value,
        (
            collections.abc.MutableSequence,
            collections.abc.MutableMapping,
            collections.abc.MutableSet,
            bytearray,
        ),
    ):
        return True
    array_flags = getattr(value, "flags", None)
    return getattr(array_flags, "writeable", False) is True
