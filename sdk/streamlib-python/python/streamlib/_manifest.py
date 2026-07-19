# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Minimal hand-rolled YAML reader for `streamlib.yaml` package metadata.

Reads only the top-level `package: { org, name, version, ... }` block — the
package identity the `@processor` decorator needs to compose a structured
[`SchemaIdent`][streamlib.schema_ident.SchemaIdent].

The `processors:` list is NOT read here: the decorator is the truth-source
for which processors a package declares (extraction is import — see
[`streamlib.extract_processors`][]). Only the package identity lives in the
manifest; the processor set is derived from `@processor` usage in code.

A full YAML parser would require PyYAML, which would be the SDK's first
runtime dep. The `package:` block shape is constrained enough that a focused
line-oriented reader suffices: 2-space indented mappings, comments, blank
lines, optional single/double quotes around scalars. Anything more complex
(anchors, multi-line strings, flow style outside `{ a: b }`) is out of scope
for this reader.

The full manifest is parsed by the Rust runtime via `serde_yaml` when the
host loads it; this reader only needs the package identity fields.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Optional, Tuple


@dataclass(frozen=True)
class ManifestPackage:
    """Resolved `package:` block from streamlib.yaml."""

    org: str
    name: str
    version: str


class ManifestParseError(ValueError):
    """Raised when a streamlib.yaml cannot be parsed for decorator use."""


def read_package_block(path: Path) -> ManifestPackage:
    """Parse the `package:` block from a streamlib.yaml.

    Raises:
        FileNotFoundError: if `path` does not exist.
        ManifestParseError: if the manifest is missing `package:`, or missing
            any of `org`/`name`/`version`, or malformed.
    """
    if not path.exists():
        raise FileNotFoundError(str(path))

    text = path.read_text(encoding="utf-8")
    org, name, version = _scan_package(text)

    missing = [field for field, val in (("org", org), ("name", name), ("version", version)) if val is None]
    if missing:
        raise ManifestParseError(
            f"streamlib.yaml at {path} is missing required `package:` field(s): "
            f"{', '.join(missing)}. The decorator requires `package: {{ org, name, version }}` "
            f"to construct a structured SchemaIdent."
        )

    return ManifestPackage(org=org, name=name, version=version)


def _scan_package(text: str) -> Tuple[Optional[str], Optional[str], Optional[str]]:
    org: Optional[str] = None
    name: Optional[str] = None
    version: Optional[str] = None

    in_package = False

    for raw in text.splitlines():
        line = _strip_comment(raw).rstrip()
        if not line.strip():
            continue

        indent = len(line) - len(line.lstrip())
        content = line.lstrip()

        if indent == 0:
            in_package = content.rstrip(":") == "package" and content.endswith(":")
            continue

        if in_package and indent == 2 and ":" in content:
            key, _, val = content.partition(":")
            key = key.strip()
            val = _strip_quotes(val.strip())
            if key == "org":
                org = val
            elif key == "name":
                name = val
            elif key == "version":
                version = val

    return org, name, version


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
