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
import inspect
import sys
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from types import ModuleType
from typing import Any

ENGINE_MODULE_NAME = "tatolab.runtime._engine"

ENGINE_STUB_PATH = (
    Path(__file__).resolve().parents[1] / "python" / "tatolab" / "runtime" / "_engine.pyi"
)

# Dunders that are part of a class's contract when it defines them; every
# other dunder is `object`'s, `Protocol`'s, or pyo3's own machinery.
CONTRACT_DUNDER_NAMES = frozenset(
    {
        "__aenter__",
        "__aexit__",
        "__aiter__",
        "__anext__",
        "__bool__",
        "__buffer__",
        "__call__",
        "__contains__",
        "__delitem__",
        "__dlpack__",
        "__dlpack_device__",
        "__enter__",
        "__exit__",
        "__getitem__",
        "__iter__",
        "__len__",
        "__next__",
        "__release_buffer__",
        "__repr__",
        "__reversed__",
        "__setitem__",
        "__str__",
    }
)

@dataclass(frozen=True)
class ConformanceFinding:
    """One way a native name and its declaration disagree, or a name held wrongly."""

    held_name: str
    disagreement: str

    def __str__(self) -> str:
        return f"{self.held_name}: {self.disagreement}"


@dataclass(frozen=True)
class EngineStubDeclaration:
    """One top-level name `_engine.pyi` declares, and how."""

    declared_as: str
    annotation_source: str | None = None


def _is_contract_member_name(member_name: str) -> bool:
    return not member_name.startswith("_") or member_name in CONTRACT_DUNDER_NAMES


def _member_kind_on_native_class(raw_member: Any) -> str:
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


def _member_kind_on_protocol(raw_member: Any) -> str:
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
        if _is_contract_member_name(member_name)
    }


def _contract_members_of_protocol(protocol: type) -> dict[str, Any]:
    members = {
        member_name: raw_member
        for member_name, raw_member in vars(protocol).items()
        if _is_contract_member_name(member_name)
    }
    for annotated_name in vars(protocol).get("__annotations__", {}):
        if _is_contract_member_name(annotated_name):
            members.setdefault(annotated_name, None)
    return members


def _parameter_shape(
    signature: inspect.Signature, drop_receiver: bool
) -> list[tuple[str, inspect._ParameterKind, bool]]:
    parameters = list(signature.parameters.values())
    if drop_receiver:
        parameters = parameters[1:]
    return [
        (parameter.name, parameter.kind, parameter.default is not inspect.Parameter.empty)
        for parameter in parameters
    ]


def _render_parameter_shape(shape: list[tuple[str, inspect._ParameterKind, bool]]) -> str:
    rendered: list[str] = []
    emitted_keyword_only_marker = False
    for index, (name, kind, has_default) in enumerate(shape):
        if kind is inspect.Parameter.KEYWORD_ONLY and not emitted_keyword_only_marker:
            rendered.append("*")
            emitted_keyword_only_marker = True
        if kind is inspect.Parameter.VAR_POSITIONAL:
            rendered.append(f"*{name}")
            emitted_keyword_only_marker = True
        elif kind is inspect.Parameter.VAR_KEYWORD:
            rendered.append(f"**{name}")
        else:
            rendered.append(f"{name}=…" if has_default else name)
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
    native_shape = _parameter_shape(native_signature, drop_receiver)
    declared_shape = _parameter_shape(declared_signature, drop_receiver)
    if native_shape == declared_shape:
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
    protocol: type, native_class: type, held_name: str | None = None
) -> list[ConformanceFinding]:
    """Every member `native_class` and `protocol` disagree on."""
    class_name = held_name or native_class.__name__
    native_members = _contract_members_of_native_class(native_class)
    protocol_members = _contract_members_of_protocol(protocol)
    findings: list[ConformanceFinding] = []
    for member_name in sorted(native_members.keys() - protocol_members.keys()):
        findings.append(
            ConformanceFinding(
                f"{class_name}.{member_name}",
                "on the native class and missing from the Protocol",
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
    python_function: Callable[..., Any], native_callable: Any, held_name: str
) -> list[ConformanceFinding]:
    """Whether a runtime-backed function's declared signature is its native callable's."""
    return _signature_findings(held_name, native_callable, python_function, drop_receiver=False)


def protocols_tatolab_stream_publishes(stream_package: ModuleType) -> dict[str, type]:
    """Every `typing.Protocol` class `tatolab.stream` lists in `__all__`, by name."""
    return {
        exported_name: exported
        for exported_name in getattr(stream_package, "__all__", [])
        if inspect.isclass(exported := getattr(stream_package, exported_name))
        and getattr(exported, "_is_protocol", False)
    }


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


def _record_stub_declarations(
    statements: Iterable[ast.stmt],
    declarations_by_name: dict[str, EngineStubDeclaration],
) -> None:
    for statement in statements:
        if isinstance(statement, ast.If):
            platform_condition_holds = _sys_platform_condition_holds_here(statement.test)
            if platform_condition_holds is not False:
                _record_stub_declarations(statement.body, declarations_by_name)
            if platform_condition_holds is not True:
                _record_stub_declarations(statement.orelse, declarations_by_name)
        elif isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef)):
            declarations_by_name[statement.name] = EngineStubDeclaration("function")
        elif isinstance(statement, ast.ClassDef):
            declarations_by_name[statement.name] = EngineStubDeclaration("class")
        elif isinstance(statement, ast.AnnAssign) and isinstance(statement.target, ast.Name):
            declarations_by_name[statement.target.id] = EngineStubDeclaration(
                "annotated name", ast.unparse(statement.annotation)
            )


def declarations_in_engine_stub(stub_source: str) -> dict[str, EngineStubDeclaration]:
    """Every top-level name `_engine.pyi` declares on this platform, by name."""
    declarations_by_name: dict[str, EngineStubDeclaration] = {}
    _record_stub_declarations(ast.parse(stub_source).body, declarations_by_name)
    return declarations_by_name


def _is_typed_as_the_class_of_its_protocol(
    held_name: str, declaration: EngineStubDeclaration
) -> bool:
    if declaration.declared_as != "annotated name" or declaration.annotation_source is None:
        return False
    annotation = declaration.annotation_source
    return annotation.startswith("type[") and (
        annotation == f"type[{held_name}]" or annotation.endswith(f".{held_name}]")
    )


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
        and (inspect.isclass(exported) or callable(exported))
    }
    held_by_tatolab_stream = set(protocols_by_name) | set(
        native_names_runtime_backed_functions_forward_to
    )
    stub_declarations = declarations_in_engine_stub(stub_source)
    held_by_stub = set(stub_declarations)
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
            if exported_name in protocols_by_name and _is_typed_as_the_class_of_its_protocol(
                exported_name, stub_declarations[exported_name]
            ):
                continue
            findings.append(
                ConformanceFinding(
                    exported_name,
                    "declared in tatolab.stream and in _engine.pyi; the stub may "
                    "name a Protocol-held class only as `type[<its Protocol>]`",
                )
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
    runtime_backed_function_registry: Iterable[Any],
    stub_source: str,
) -> list[ConformanceFinding]:
    """Every finding for the engine against `tatolab.stream` and `_engine.pyi`."""
    protocols_by_name = protocols_tatolab_stream_publishes(stream_package)
    registrations = list(runtime_backed_function_registry)
    findings: list[ConformanceFinding] = []
    for protocol_name, protocol in sorted(protocols_by_name.items()):
        native_class = getattr(engine_module, protocol_name, None)
        if inspect.isclass(native_class):
            findings.extend(conformance_findings_for_protocol(protocol, native_class))
    for registration in registrations:
        native_callable = getattr(engine_module, registration.native_callable_name, None)
        held_name = registration.python_function.__name__
        if native_callable is None:
            continue
        findings.extend(
            conformance_findings_for_runtime_backed_function(
                registration.python_function, native_callable, held_name
            )
        )
    findings.extend(
        holding_findings(
            engine_module,
            protocols_by_name,
            (registration.native_callable_name for registration in registrations),
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
    held_names -= set(declarations_in_engine_stub(stub_source))
    escaped_module_name = ENGINE_MODULE_NAME.replace(".", r"\.")
    return [f"{escaped_module_name}\\.__all__"] + [
        f"{escaped_module_name}\\.{held_name}(\\..*)?" for held_name in sorted(held_names)
    ]


def _real_inputs() -> tuple[ModuleType, ModuleType, list[Any], str]:
    import importlib

    import tatolab.stream
    from tatolab.stream._runtime_lend import runtime_backed_function_registry

    engine_module = importlib.import_module(ENGINE_MODULE_NAME)
    return (
        engine_module,
        tatolab.stream,
        list(runtime_backed_function_registry),
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
    engine_module, stream_package, registrations, stub_source = _real_inputs()
    if parsed.print_stubtest_allowlist:
        for allowlist_entry in stubtest_allowlist_of_names_held_by_tatolab_stream(
            protocols_tatolab_stream_publishes(stream_package),
            (registration.native_callable_name for registration in registrations),
            stub_source,
        ):
            sys.stdout.write(f"{allowlist_entry}\n")
        return 0
    findings = all_conformance_findings(engine_module, stream_package, registrations, stub_source)
    for finding in findings:
        sys.stdout.write(f"{finding}\n")
    if findings:
        sys.stdout.write(f"{len(findings)} conformance finding(s)\n")
        return 1
    sys.stdout.write("every runtime-backed name conforms\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
