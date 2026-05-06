# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Minimal hand-rolled YAML reader for `streamlib.yaml` package metadata.

Reads only the shape needed by the `@processor` / `@schema` decorators:

- top-level `package: { org, name, version, ... }` block
- top-level `processors:` list, returning the `name` of each entry

A full YAML parser would require PyYAML, which would be the SDK's first
runtime dep. The streamlib.yaml shape is constrained enough that a focused
line-oriented reader suffices: 2-space indented mappings, sequence items
prefixed by `-`, comments, blank lines, optional single/double quotes
around scalars. Anything more complex (anchors, multi-line strings, flow
style outside `{ a: b }`) is out of scope for this reader.

The full manifest is parsed by the Rust runtime via `serde_yaml` when the
host loads it; this reader only needs enough fields to validate the
decorator's short name and compose a structured `SchemaIdent`.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional, Tuple


@dataclass(frozen=True)
class ManifestPackage:
    """Resolved `package:` block from streamlib.yaml."""

    org: str
    name: str
    version: str


@dataclass(frozen=True)
class ManifestSummary:
    """Decorator-side view of a streamlib.yaml.

    Carries the package metadata and the set of processor names declared
    in the manifest — enough for the decorator to validate that its
    short-name argument matches an entry the manifest declares.
    """

    package: ManifestPackage
    processor_names: List[str]


class ManifestParseError(ValueError):
    """Raised when a streamlib.yaml cannot be parsed for decorator use."""


def read_manifest_summary(path: Path) -> ManifestSummary:
    """Parse the package block + processors[].name list from a streamlib.yaml.

    Raises:
        FileNotFoundError: if `path` does not exist.
        ManifestParseError: if the manifest is missing `package:`, missing
            any of `org`/`name`/`version`, or malformed.
    """
    if not path.exists():
        raise FileNotFoundError(str(path))

    text = path.read_text(encoding="utf-8")
    org, name, version, processor_names = _scan(text, path)

    missing = [field for field, val in (("org", org), ("name", name), ("version", version)) if val is None]
    if missing:
        raise ManifestParseError(
            f"streamlib.yaml at {path} is missing required `package:` field(s): "
            f"{', '.join(missing)}. The decorator requires `package: {{ org, name, version }}` "
            f"to construct a structured SchemaIdent."
        )

    return ManifestSummary(
        package=ManifestPackage(org=org, name=name, version=version),
        processor_names=processor_names,
    )


def _scan(
    text: str, path: Path
) -> Tuple[Optional[str], Optional[str], Optional[str], List[str]]:
    org: Optional[str] = None
    name: Optional[str] = None
    version: Optional[str] = None
    processor_names: List[str] = []

    section: Optional[str] = None  # "package" | "processors" | None

    for raw in text.splitlines():
        line = _strip_comment(raw).rstrip()
        if not line.strip():
            continue

        indent = len(line) - len(line.lstrip())
        content = line.lstrip()

        if indent == 0:
            if content.rstrip(":") == "package" and content.endswith(":"):
                section = "package"
            elif content.rstrip(":") == "processors" and content.endswith(":"):
                section = "processors"
            else:
                section = None
            continue

        if section == "package" and indent == 2 and ":" in content:
            key, _, val = content.partition(":")
            key = key.strip()
            val = _strip_quotes(val.strip())
            if key == "org":
                org = val
            elif key == "name":
                name = val
            elif key == "version":
                version = val

        elif section == "processors" and indent == 2 and content.startswith("- "):
            # Sequence item start. We assume `name:` is the first key per the
            # repo convention; if a future manifest reorders, extend this.
            rest = content[2:].lstrip()
            if rest.startswith("name:"):
                _, _, val = rest.partition(":")
                val = _strip_quotes(val.strip())
                if val:
                    processor_names.append(val)
            else:
                raise ManifestParseError(
                    f"streamlib.yaml at {path} has a processor entry whose first "
                    f"key is not `name:` — the decorator's manifest reader only "
                    f"supports `name`-first ordering. Got: {content!r}"
                )

    return org, name, version, processor_names


def _strip_quotes(s: str) -> str:
    """Strip a single surrounding pair of double or single quotes."""
    if len(s) >= 2 and ((s[0] == s[-1] == '"') or (s[0] == s[-1] == "'")):
        return s[1:-1]
    return s


def _strip_comment(s: str) -> str:
    """Strip a trailing `# ...` comment when `#` is whitespace-preceded.

    Plays it safe around `#` characters embedded in string values: only
    strips when the `#` follows whitespace and is outside of a quoted
    region.
    """
    in_quote: Optional[str] = None
    for i, c in enumerate(s):
        if c in ('"', "'"):
            if in_quote is None:
                in_quote = c
            elif in_quote == c:
                in_quote = None
        elif c == "#" and in_quote is None:
            if i == 0 or s[i - 1].isspace():
                return s[:i].rstrip()
    return s
