# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Hold the engine's native classes and functions to what `tatolab.stream` declares.

What a node is handed while it runs is declared once, in `tatolab.stream`: a
class as a `typing.Protocol`, a function as a runtime-backed function. The
native module implements them. This gate compares the two through the
signatures pyo3 publishes (`__text_signature__`), member for member, and holds
every class and callable `tatolab.runtime._engine` exports to exactly one
declaration — a `tatolab.stream` Protocol or runtime-backed function, or an
entry in `_engine.pyi` — never both and never neither.

Run as a script it prints its findings and exits non-zero on any.
`--print-stubtest-allowlist` prints instead the `mypy.stubtest` allowlist of the
names it holds, which stubtest then skips.
"""

from __future__ import annotations

import argparse
import ast
import importlib
import inspect
import pkgutil
import sys
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType
from typing import Any, Generic, Literal, Protocol, TypeVar

ENGINE_MODULE_NAME = "tatolab.runtime._engine"

ENGINE_STUB_PATH = (
    Path(__file__).resolve().parents[1] / "python" / "tatolab" / "runtime" / "_engine.pyi"
)

# The dunders pyo3 puts in every pyclass's own `__dict__`.
PYO3_CLASS_MACHINERY_DUNDER_NAMES = frozenset(
    {"__dict__", "__doc__", "__module__", "__weakref__"}
)

NATIVE_CONSTRUCTOR_NAME = "__new__"

# How a shape spells a default pyo3 publishes but cannot spell: `...` in its
# `__text_signature__`.
DEFAULT_PYO3_CANNOT_SPELL = "…"

_ProtocolTypeParameter = TypeVar("_ProtocolTypeParameter", covariant=True)


class _ProtocolDeclaringNothing(Protocol):
    pass


class _GenericProtocolDeclaringNothing(Protocol[_ProtocolTypeParameter]):
    pass


# The dunders `typing.Protocol` puts in a Protocol's own `__dict__`, read off two
# that declare nothing rather than listed, because they differ across Python
# versions.
PROTOCOL_MACHINERY_DUNDER_NAMES = frozenset(vars(_ProtocolDeclaringNothing)) | frozenset(
    vars(_GenericProtocolDeclaringNothing)
)

_PROTOCOL_MACHINERY_BASES: tuple[Any, ...] = (Protocol, Generic, object)

MemberKind = Literal["staticmethod", "classmethod", "property", "method", "attribute"]

ParameterShape = list[tuple[str, inspect._ParameterKind, "str | None"]]


@dataclass(frozen=True)
class ConformanceFinding:
    """One way a native name and its declaration disagree, or a name held wrongly."""

    held_name: str
    disagreement: str

    def __str__(self) -> str:
        return f"{self.held_name}: {self.disagreement}"


def is_contract_member_name(member_name: str, machinery_dunder_names: frozenset[str]) -> bool:
    """Whether a member is part of a class's contract: public, or a dunder beyond machinery."""
    if not member_name.startswith("_"):
        return True
    return (
        member_name.startswith("__")
        and member_name.endswith("__")
        and member_name not in machinery_dunder_names
    )


def _member_kind_on_native_class(raw_member: Any) -> MemberKind:
    if isinstance(raw_member, staticmethod):
        return "staticmethod"
    if isinstance(raw_member, classmethod) or type(raw_member).__name__ == "classmethod_descriptor":
        return "classmethod"
    if isinstance(raw_member, property) or inspect.isgetsetdescriptor(raw_member) or (
        inspect.ismemberdescriptor(raw_member)
    ):
        return "property"
    if callable(raw_member):
        return "method"
    return "attribute"


def _member_kind_on_protocol(raw_member: Any) -> MemberKind:
    if isinstance(raw_member, staticmethod):
        return "staticmethod"
    if isinstance(raw_member, classmethod):
        return "classmethod"
    if isinstance(raw_member, property):
        return "property"
    if inspect.isfunction(raw_member):
        return "method"
    return "attribute"


def _contract_members_of_native_class(native_class: type) -> dict[str, Any]:
    return {
        member_name: raw_member
        for member_name, raw_member in vars(native_class).items()
        if is_contract_member_name(member_name, PYO3_CLASS_MACHINERY_DUNDER_NAMES)
    }


def contract_members_of_protocol(protocol: type) -> dict[str, Any]:
    """A Protocol's contract members by name: its own and every Protocol base's."""
    members: dict[str, Any] = {}
    for declaring_class in reversed(protocol.__mro__):
        if declaring_class in _PROTOCOL_MACHINERY_BASES:
            continue
        for member_name, raw_member in vars(declaring_class).items():
            if is_contract_member_name(member_name, PROTOCOL_MACHINERY_DUNDER_NAMES):
                members[member_name] = raw_member
        for annotated_name in inspect.get_annotations(declaring_class):
            if is_contract_member_name(annotated_name, PROTOCOL_MACHINERY_DUNDER_NAMES):
                members.setdefault(annotated_name, None)
    return members


def _default_spelling(default: Any, spelled_by_pyo3: bool) -> str | None:
    if default is inspect.Parameter.empty:
        return None
    if spelled_by_pyo3 and default is Ellipsis:
        return DEFAULT_PYO3_CANNOT_SPELL
    return repr(default)


def _parameter_shape(
    signature: inspect.Signature, drop_receiver: bool, spelled_by_pyo3: bool
) -> ParameterShape:
    parameters = list(signature.parameters.values())
    if drop_receiver:
        parameters = parameters[1:]
    return [
        (parameter.name, parameter.kind, _default_spelling(parameter.default, spelled_by_pyo3))
        for parameter in parameters
    ]


def _parameter_shapes_agree(native_shape: ParameterShape, declared_shape: ParameterShape) -> bool:
    if len(native_shape) != len(declared_shape):
        return False
    for (native_name, native_kind, native_default), (
        declared_name,
        declared_kind,
        declared_default,
    ) in zip(native_shape, declared_shape):
        if native_name != declared_name or native_kind is not declared_kind:
            return False
        if native_default == DEFAULT_PYO3_CANNOT_SPELL:
            if declared_default is None:
                return False
        elif native_default != declared_default:
            return False
    return True


def _render_parameter_shape(shape: ParameterShape) -> str:
    rendered: list[str] = []
    emitted_keyword_only_marker = False
    for index, (name, kind, default_spelling) in enumerate(shape):
        if kind is inspect.Parameter.KEYWORD_ONLY and not emitted_keyword_only_marker:
            rendered.append("*")
            emitted_keyword_only_marker = True
        if kind is inspect.Parameter.VAR_POSITIONAL:
            rendered.append(f"*{name}")
            emitted_keyword_only_marker = True
        elif kind is inspect.Parameter.VAR_KEYWORD:
            rendered.append(f"**{name}")
        else:
            rendered.append(name if default_spelling is None else f"{name}={default_spelling}")
        next_kind = shape[index + 1][1] if index + 1 < len(shape) else None
        if kind is inspect.Parameter.POSITIONAL_ONLY and (
            next_kind is not inspect.Parameter.POSITIONAL_ONLY
        ):
            rendered.append("/")
    return f"({', '.join(rendered)})"


def _signature_or_refusal(callable_object: Any) -> inspect.Signature | str:
    try:
        return inspect.signature(callable_object)
    except (TypeError, ValueError) as unpublished:
        return f"publishes no signature ({unpublished})"


def _signature_findings(
    held_name: str,
    native_callable: Any,
    declared_callable: Any,
    drop_receiver: bool,
) -> list[ConformanceFinding]:
    native_signature = _signature_or_refusal(native_callable)
    declared_signature = _signature_or_refusal(declared_callable)
    if isinstance(native_signature, str):
        return [ConformanceFinding(held_name, f"the native callable {native_signature}")]
    if isinstance(declared_signature, str):
        return [ConformanceFinding(held_name, f"the declaration {declared_signature}")]
    native_shape = _parameter_shape(native_signature, drop_receiver, spelled_by_pyo3=True)
    declared_shape = _parameter_shape(declared_signature, drop_receiver, spelled_by_pyo3=False)
    if _parameter_shapes_agree(native_shape, declared_shape):
        return []
    return [
        ConformanceFinding(
            held_name,
            "signature differs: native "
            f"{_render_parameter_shape(native_shape)}, declared "
            f"{_render_parameter_shape(declared_shape)}",
        )
    ]


def conformance_findings_for_protocol(
    protocol: type,
    native_class: type,
    *,
    native_constructor_is_held_by_a_runtime_backed_function: bool,
) -> list[ConformanceFinding]:
    """Every member `native_class` and `protocol` disagree on.

    A native constructor is a member too, unless a runtime-backed function
    forwards to it and so holds its signature.
    """
    class_name = native_class.__name__
    native_members = _contract_members_of_native_class(native_class)
    if native_constructor_is_held_by_a_runtime_backed_function:
        native_members.pop(NATIVE_CONSTRUCTOR_NAME, None)
    protocol_members = contract_members_of_protocol(protocol)
    findings: list[ConformanceFinding] = []
    for member_name in sorted(native_members.keys() - protocol_members.keys()):
        findings.append(
            ConformanceFinding(
                f"{class_name}.{member_name}",
                "a native constructor no runtime-backed function forwards to"
                if member_name == NATIVE_CONSTRUCTOR_NAME
                else "on the native class and missing from the Protocol",
            )
        )
    for member_name in sorted(protocol_members.keys() - native_members.keys()):
        findings.append(
            ConformanceFinding(
                f"{class_name}.{member_name}",
                "declared by the Protocol and missing from the native class",
            )
        )
    for member_name in sorted(native_members.keys() & protocol_members.keys()):
        member_held_name = f"{class_name}.{member_name}"
        native_kind = _member_kind_on_native_class(native_members[member_name])
        protocol_kind = _member_kind_on_protocol(protocol_members[member_name])
        if native_kind != protocol_kind:
            findings.append(
                ConformanceFinding(
                    member_held_name,
                    f"a {native_kind} on the native class, a {protocol_kind} on the Protocol",
                )
            )
            continue
        if native_kind in ("property", "attribute"):
            continue
        raw_protocol_member = protocol_members[member_name]
        if native_kind == "staticmethod":
            findings.extend(
                _signature_findings(
                    member_held_name,
                    getattr(native_class, member_name),
                    raw_protocol_member.__func__,
                    drop_receiver=False,
                )
            )
        elif native_kind == "classmethod":
            findings.extend(
                _signature_findings(
                    member_held_name,
                    getattr(native_class, member_name),
                    getattr(protocol, member_name),
                    drop_receiver=False,
                )
            )
        else:
            findings.extend(
                _signature_findings(
                    member_held_name,
                    native_members[member_name],
                    raw_protocol_member,
                    drop_receiver=True,
                )
            )
    return findings


def conformance_findings_for_runtime_backed_function(
    runtime_backed_function: Callable[..., Any], native_callable: Any, held_name: str
) -> list[ConformanceFinding]:
    """Whether a runtime-backed function's declared signature is its native callable's."""
    return _signature_findings(
        held_name, native_callable, runtime_backed_function, drop_receiver=False
    )


def runtime_backed_protocols_tatolab_stream_declares(
    stream_package: ModuleType,
) -> dict[str, type]:
    """Every Protocol a module of `tatolab.stream` declares `@runtime_backed_protocol`, by name.

    Every module is imported first, so a Protocol no import chain reaches is
    registered all the same.
    """
    for submodule in pkgutil.iter_modules(stream_package.__path__, f"{stream_package.__name__}."):
        importlib.import_module(submodule.name)
    runtime_lend_module = importlib.import_module(f"{stream_package.__name__}._runtime_lend")
    return dict(runtime_lend_module.runtime_backed_protocol_registry)


def _sys_platform_condition_holds_here(condition: ast.expr) -> bool | None:
    """Evaluate a stub's `sys.platform == "<name>"` branch here; `None` for any other test."""
    if not (
        isinstance(condition, ast.Compare)
        and len(condition.ops) == 1
        and isinstance(condition.ops[0], (ast.Eq, ast.NotEq))
        and ast.unparse(condition.left) == "sys.platform"
        and isinstance(condition.comparators[0], ast.Constant)
    ):
        return None
    platform_matches = sys.platform == condition.comparators[0].value
    return platform_matches if isinstance(condition.ops[0], ast.Eq) else not platform_matches


def _record_stub_declared_names(
    statements: Iterable[ast.stmt], declared_names: set[str]
) -> None:
    for statement in statements:
        if isinstance(statement, ast.If):
            platform_condition_holds = _sys_platform_condition_holds_here(statement.test)
            if platform_condition_holds is not False:
                _record_stub_declared_names(statement.body, declared_names)
            if platform_condition_holds is not True:
                _record_stub_declared_names(statement.orelse, declared_names)
        elif isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            declared_names.add(statement.name)
        elif isinstance(statement, ast.AnnAssign) and isinstance(statement.target, ast.Name):
            declared_names.add(statement.target.id)


def names_declared_in_engine_stub(stub_source: str) -> set[str]:
    """Every top-level name `_engine.pyi` declares on this platform."""
    declared_names: set[str] = set()
    _record_stub_declared_names(ast.parse(stub_source).body, declared_names)
    return declared_names


def holding_findings(
    engine_module: ModuleType,
    protocols_by_name: Mapping[str, type],
    native_names_runtime_backed_functions_forward_to: Iterable[str],
    stub_source: str,
) -> list[ConformanceFinding]:
    """Every engine export held by no declaration or by two, and every declaration of nothing."""
    exported_names = {
        exported_name
        for exported_name, exported in vars(engine_module).items()
        if not exported_name.startswith("_")
        and callable(exported)
    }
    held_by_tatolab_stream = set(protocols_by_name) | set(
        native_names_runtime_backed_functions_forward_to
    )
    held_by_stub = names_declared_in_engine_stub(stub_source)
    findings: list[ConformanceFinding] = []
    for exported_name in sorted(exported_names):
        in_stream = exported_name in held_by_tatolab_stream
        in_stub = exported_name in held_by_stub
        if not in_stream and not in_stub:
            findings.append(
                ConformanceFinding(
                    exported_name,
                    "exported by the engine and declared nowhere: neither a "
                    "tatolab.stream Protocol or runtime-backed function nor _engine.pyi",
                )
            )
        elif in_stream and in_stub:
            findings.append(
                ConformanceFinding(exported_name, "declared in tatolab.stream and in _engine.pyi")
            )
    for declared_name in sorted((held_by_tatolab_stream | held_by_stub) - exported_names):
        findings.append(
            ConformanceFinding(
                declared_name,
                "declared but not exported by the engine",
            )
        )
    return findings


def all_conformance_findings(
    engine_module: ModuleType,
    stream_package: ModuleType,
    native_callable_name_of_each_runtime_backed_function: Mapping[Callable[..., Any], str],
    stub_source: str,
) -> list[ConformanceFinding]:
    """Every finding for the engine against `tatolab.stream` and `_engine.pyi`."""
    protocols_by_name = runtime_backed_protocols_tatolab_stream_declares(stream_package)
    native_names_runtime_backed_functions_forward_to = set(
        native_callable_name_of_each_runtime_backed_function.values()
    )
    findings: list[ConformanceFinding] = []
    for protocol_name, protocol in sorted(protocols_by_name.items()):
        native_class = getattr(engine_module, protocol_name, None)
        if inspect.isclass(native_class):
            findings.extend(
                conformance_findings_for_protocol(
                    protocol,
                    native_class,
                    native_constructor_is_held_by_a_runtime_backed_function=(
                        protocol_name in native_names_runtime_backed_functions_forward_to
                    ),
                )
            )
    for (
        runtime_backed_function,
        native_callable_name,
    ) in native_callable_name_of_each_runtime_backed_function.items():
        native_callable = getattr(engine_module, native_callable_name, None)
        if native_callable is None:
            continue
        findings.extend(
            conformance_findings_for_runtime_backed_function(
                runtime_backed_function, native_callable, runtime_backed_function.__name__
            )
        )
    findings.extend(
        holding_findings(
            engine_module,
            protocols_by_name,
            native_names_runtime_backed_functions_forward_to,
            stub_source,
        )
    )
    return findings


def stubtest_allowlist_of_names_held_by_tatolab_stream(
    protocols_by_name: Mapping[str, type],
    native_names_runtime_backed_functions_forward_to: Iterable[str],
    stub_source: str,
) -> list[str]:
    """The `mypy.stubtest` allowlist: each name `tatolab.stream` alone declares, and `__all__`.

    The stub's `__all__` lists only what the stub declares, so it never matches
    the module's; this gate's holding check is what keeps the exports whole.
    """
    held_names = set(protocols_by_name) | set(native_names_runtime_backed_functions_forward_to)
    held_names -= names_declared_in_engine_stub(stub_source)
    escaped_module_name = ENGINE_MODULE_NAME.replace(".", r"\.")
    return [f"{escaped_module_name}\\.__all__"] + [
        f"{escaped_module_name}\\.{held_name}(\\..*)?" for held_name in sorted(held_names)
    ]


def _real_inputs() -> tuple[ModuleType, ModuleType, dict[Callable[..., Any], str], str]:
    import tatolab.stream
    from tatolab.stream._runtime_lend import runtime_backed_function_registry

    engine_module = importlib.import_module(ENGINE_MODULE_NAME)
    return (
        engine_module,
        tatolab.stream,
        dict(runtime_backed_function_registry),
        ENGINE_STUB_PATH.read_text(),
    )


def main(arguments: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    parser.add_argument(
        "--print-stubtest-allowlist",
        action="store_true",
        help="print the mypy.stubtest allowlist of the names tatolab.stream holds",
    )
    parsed = parser.parse_args(arguments)
    (
        engine_module,
        stream_package,
        native_callable_name_of_each_runtime_backed_function,
        stub_source,
    ) = _real_inputs()
    if parsed.print_stubtest_allowlist:
        for allowlist_entry in stubtest_allowlist_of_names_held_by_tatolab_stream(
            runtime_backed_protocols_tatolab_stream_declares(stream_package),
            native_callable_name_of_each_runtime_backed_function.values(),
            stub_source,
        ):
            sys.stdout.write(f"{allowlist_entry}\n")
        return 0
    findings = all_conformance_findings(
        engine_module,
        stream_package,
        native_callable_name_of_each_runtime_backed_function,
        stub_source,
    )
    for finding in findings:
        sys.stdout.write(f"{finding}\n")
    if findings:
        sys.stdout.write(f"{len(findings)} conformance finding(s)\n")
        return 1
    sys.stdout.write("every runtime-backed name conforms\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
