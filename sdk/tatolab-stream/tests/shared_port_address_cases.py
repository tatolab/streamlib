# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The port address cases the engine's `PortAddress` and this package's parse are both held to."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

SHARED_PORT_ADDRESS_CASES_PATH = (
    Path(__file__).resolve().parents[3]
    / "runtime"
    / "streamlib-engine"
    / "tests"
    / "fixtures"
    / "port_address_cases.json"
)

_shared_port_address_cases: "list[dict[str, Any]]" = json.loads(
    SHARED_PORT_ADDRESS_CASES_PATH.read_text(encoding="utf-8")
)

PORT_ADDRESS_CASES_THAT_PARSE = [
    case for case in _shared_port_address_cases if not case.get("refused")
]
"""`{address, machine?, stream?, node, port}`: the chunks each address parses to, cast."""

PORT_ADDRESS_CASES_THAT_ARE_REFUSED = [
    case for case in _shared_port_address_cases if case.get("refused")
]
"""`{address, refused: true}`: an address naming no port."""


def port_address_case_id(case: "dict[str, Any]") -> str:
    """The case's address as a pytest id, quoted so an empty one still reads."""
    return repr(case["address"])
