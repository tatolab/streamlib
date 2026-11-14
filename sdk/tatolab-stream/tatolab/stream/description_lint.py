# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# streamlib:lint-logging:allow-file — the findings are the command's report on stdout,
# not log events.

"""The description lint: which node types, ports and config keys have no description.

Off unless a project runs it, and the runtime refuses nothing it finds. Run it as
`python -m tatolab.stream.description_lint nodes`, naming the packages or modules that
declare node types; it imports each one and every submodule, and exits 1 when it finds
anything. A test can call `lint_the_descriptions_of_the_node_types_in` instead. A
description that writes raw HTML is a finding too: a description is CommonMark with no
raw HTML.
"""

from __future__ import annotations

import dataclasses
import importlib
import pkgutil
import re
import sys
from collections.abc import Iterable, Iterator, Sequence
from types import ModuleType
from typing import Any, Optional

from ._node_declaration import (
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_DECLARED_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

__all__ = [
    "DescriptionLintFinding",
    "lint_the_descriptions_of_a_node_type",
    "lint_the_descriptions_of_the_node_types_in",
    "main",
    "writes_raw_html",
]

MISSING_DESCRIPTION = "missing"
RAW_HTML_DESCRIPTION = "raw_html"

_FENCE_PATTERN = re.compile(r"^\s*(```|~~~)")
_CODE_SPAN_PATTERN = re.compile(r"`[^`]*`")
_RAW_HTML_PATTERN = re.compile(r"<(?:[!?]|/?[A-Za-z][A-Za-z0-9-]*(?:[\s/>]))")


@dataclasses.dataclass(frozen=True)
class DescriptionLintFinding:
    """One description a node type leaves out, or one that writes raw HTML."""

    node_type: str
    """The node type, as `<module>:<qualname>`."""

    declaration: str
    """What the description is declared on: `the node type`, `input port \\`video\\``, …"""

    problem: str
    """`missing` or `raw_html`."""

    def __str__(self) -> str:
        if self.problem == RAW_HTML_DESCRIPTION:
            return (
                f"{self.node_type}: {self.declaration}'s description writes raw HTML, which "
                f"a description's CommonMark does not allow"
            )
        return f"{self.node_type}: {self.declaration} has no description"


def writes_raw_html(markdown: str) -> bool:
    """Whether `markdown` holds a raw HTML tag, comment or declaration outside code; an autolink is not one."""
    prose_lines: list[str] = []
    inside_a_fence = False
    for line in markdown.splitlines():
        if _FENCE_PATTERN.match(line):
            inside_a_fence = not inside_a_fence
            continue
        if not inside_a_fence:
            prose_lines.append(_CODE_SPAN_PATTERN.sub("", line))
    return _RAW_HTML_PATTERN.search("\n".join(prose_lines) + "\n") is not None


def lint_the_descriptions_of_a_node_type(declared_node: Any) -> "list[DescriptionLintFinding]":
    """Every finding on one declared node: its description, each port's and each top-level config key's."""
    node_type = f"{declared_node.__module__}:{declared_node.__qualname__}"
    described: list[tuple[str, Optional[str]]] = [
        ("the node type", getattr(declared_node, NODE_DECLARATION_DESCRIPTION_STAMP, None))
    ]
    for stamp, direction in (
        (NODE_DECLARATION_INPUT_PORTS_STAMP, "input"),
        (NODE_DECLARATION_OUTPUT_PORTS_STAMP, "output"),
    ):
        for port in getattr(declared_node, stamp, None) or []:
            described.append((f"{direction} port `{port['name']}`", port.get("description")))
    config_schema = getattr(declared_node, NODE_DECLARATION_CONFIG_SCHEMA_STAMP, None) or {}
    for config_key, key_schema in (config_schema.get("properties") or {}).items():
        described.append((f"config key `{config_key}`", key_schema.get("description")))

    findings: list[DescriptionLintFinding] = []
    for declaration, description in described:
        if not isinstance(description, str) or not description.strip():
            findings.append(DescriptionLintFinding(node_type, declaration, MISSING_DESCRIPTION))
        elif writes_raw_html(description):
            findings.append(DescriptionLintFinding(node_type, declaration, RAW_HTML_DESCRIPTION))
    return findings


def lint_the_descriptions_of_the_node_types_in(
    module_names: Iterable[str],
) -> "list[DescriptionLintFinding]":
    """Every finding on the node types each named module, and each submodule of a named package, declares."""
    findings: list[DescriptionLintFinding] = []
    seen_node_types: set[int] = set()
    for module in _modules_and_their_submodules(module_names):
        for declared_node in _nodes_a_module_declares(module):
            if id(declared_node) in seen_node_types:
                continue
            seen_node_types.add(id(declared_node))
            findings.extend(lint_the_descriptions_of_a_node_type(declared_node))
    return findings


def _modules_and_their_submodules(module_names: Iterable[str]) -> Iterator[ModuleType]:
    for module_name in module_names:
        module = importlib.import_module(module_name)
        yield module
        package_path = getattr(module, "__path__", None)
        if package_path is None:
            continue
        for submodule in pkgutil.walk_packages(package_path, prefix=f"{module.__name__}."):
            yield importlib.import_module(submodule.name)


def _nodes_a_module_declares(module: ModuleType) -> "list[Any]":
    return sorted(
        (
            value
            for value in vars(module).values()
            if getattr(value, NODE_DECLARATION_DECLARED_STAMP, False) is True
            and getattr(value, "__module__", None) == module.__name__
        ),
        key=lambda declared_node: declared_node.__qualname__,
    )


def main(arguments: "Optional[Sequence[str]]" = None) -> int:
    """Print every finding on the named modules' node types; 1 when there is one, else 0."""
    module_names = list(sys.argv[1:] if arguments is None else arguments)
    if not module_names:
        print(
            "usage: python -m tatolab.stream.description_lint <package or module> ...",
            file=sys.stderr,
        )
        return 2
    findings = lint_the_descriptions_of_the_node_types_in(module_names)
    for finding in findings:
        print(finding)
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
