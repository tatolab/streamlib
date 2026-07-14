# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Structured schema identity for the streamlib Python SDK.

Mirrors Rust's `streamlib_idents::SchemaIdent` shape: 4 validating fields
(`org`, `package`, `type_`, `version`) constructed directly. There is no
parse / from-string API — joined-string representations like
`"@org/pkg/Type@v"` are render-only (`__str__`) and never round-trip
through a parser.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Pattern

# Validation patterns mirror Rust's `streamlib_idents` newtypes.
_ORG_PATTERN: Pattern[str] = re.compile(r"^[a-z][a-z0-9-]*$")
_PACKAGE_PATTERN: Pattern[str] = re.compile(r"^[a-z][a-z0-9-]*$")
_TYPE_PATTERN: Pattern[str] = re.compile(r"^[A-Z][A-Za-z0-9]*$")
_VERSION_PATTERN: Pattern[str] = re.compile(r"^\d+\.\d+\.\d+$")


@dataclass(frozen=True)
class SchemaIdent:
    """Structured schema identifier — 4 fields, validated at construction."""

    org: str
    package: str
    type_: str
    version: str

    def __post_init__(self) -> None:
        if not isinstance(self.org, str) or not _ORG_PATTERN.match(self.org):
            raise ValueError(
                f"invalid org {self.org!r}: must match [a-z][a-z0-9-]*"
            )
        if not isinstance(self.package, str) or not _PACKAGE_PATTERN.match(self.package):
            raise ValueError(
                f"invalid package {self.package!r}: must match [a-z][a-z0-9-]*"
            )
        if not isinstance(self.type_, str) or not _TYPE_PATTERN.match(self.type_):
            raise ValueError(
                f"invalid type {self.type_!r}: must match [A-Z][A-Za-z0-9]* (PascalCase)"
            )
        if not isinstance(self.version, str) or not _VERSION_PATTERN.match(self.version):
            raise ValueError(
                f"invalid version {self.version!r}: must match major.minor.patch"
            )

    def __str__(self) -> str:
        return f"@{self.org}/{self.package}/{self.type_}@{self.version}"

    def to_wire_dict(self) -> dict:
        """Render as the IPC wire-format dict.

        Used by callers that need to hand the structured ident to JSON-RPC
        / iceoryx2 envelopes that already speak the 4-field shape.
        """
        return {
            "org": self.org,
            "package": self.package,
            "type": self.type_,
            "version": self.version,
        }
