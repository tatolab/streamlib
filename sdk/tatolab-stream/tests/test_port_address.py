# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The pure-Python port address parse against the fixture the engine's `PortAddress` is held to."""

from __future__ import annotations

import ast
import sys
from pathlib import Path
from typing import Any

import pytest

import tatolab.stream
from shared_port_address_cases import (
    PORT_ADDRESS_CASES_THAT_ARE_REFUSED,
    PORT_ADDRESS_CASES_THAT_PARSE,
    port_address_case_id,
)
from tatolab.stream._exposed_name_cast import ExposedNameCastsToNothingError
from tatolab.stream._port_address import (
    InvalidPortAddressError,
    PortAddress,
    parse_port_address,
)

PORT_ADDRESS_SOURCE = Path(tatolab.stream.__file__).resolve().parent / "_port_address.py"


@pytest.mark.parametrize("case", PORT_ADDRESS_CASES_THAT_PARSE, ids=port_address_case_id)
def test_a_fixture_address_parses_as_the_engine_parses_it(case: "dict[str, Any]") -> None:
    assert parse_port_address(case["address"]) == PortAddress(
        machine_name=case.get("machine"),
        stream_name=case.get("stream"),
        node_name=case["node"],
        port_name=case["port"],
    )


@pytest.mark.parametrize("case", PORT_ADDRESS_CASES_THAT_ARE_REFUSED, ids=port_address_case_id)
def test_a_fixture_address_naming_no_port_is_refused_naming_it(case: "dict[str, Any]") -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        parse_port_address(case["address"])

    assert repr(case["address"]) in str(refusal.value)


@pytest.mark.parametrize(
    ("address", "chunk_count"),
    [("video", 1), ("", 1), ("a/b/c/d/e", 5)],
    ids=["one-chunk", "empty", "five-chunks"],
)
def test_an_address_of_the_wrong_chunk_count_is_refused_naming_the_three_shapes(
    address: str, chunk_count: int
) -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        parse_port_address(address)

    message = str(refusal.value)
    assert message.startswith(f"the port address {address!r} has {chunk_count} chunk")
    assert "`<node>/<port>` in this stream" in message
    assert "`<stream>/<node>/<port>` on this machine" in message
    assert "`<machine>/<stream>/<node>/<port>`" in message


@pytest.mark.parametrize(
    ("address", "part", "chunk"),
    [
        ("/camera/video", "stream", ""),
        ("main//video", "node", ""),
        ("main/camera/video/", "port", ""),
        ("main/../video", "node", ".."),
        ("main/./video", "node", "."),
        ("main/!!!/video", "node", "!!!"),
        ("✨/main/camera/video", "machine", "✨"),
    ],
    ids=["leading-slash", "doubled-slash", "trailing-slash", "dot-dot", "dot", "punctuation", "machine"],
)
def test_a_chunk_naming_nothing_is_refused_naming_its_part_and_the_address(
    address: str, part: str, chunk: str
) -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        parse_port_address(address)

    assert str(refusal.value).startswith(
        f"the {part} chunk {chunk!r} of the port address {address!r} cannot name a {part}: "
    )


def test_an_empty_chunk_is_refused_naming_the_slash_that_made_it() -> None:
    with pytest.raises(InvalidPortAddressError, match="a '/' doubled, leading or trailing"):
        parse_port_address("main//video")


def test_a_chunk_casting_to_nothing_carries_the_casts_reason() -> None:
    with pytest.raises(InvalidPortAddressError) as refusal:
        parse_port_address("main/!!!/video")

    assert isinstance(refusal.value.__cause__, ExposedNameCastsToNothingError)
    assert "cannot name anything" in str(refusal.value)


def test_a_refused_address_is_a_value_error() -> None:
    assert issubclass(InvalidPortAddressError, ValueError)


def test_an_address_renders_its_cast_chunks_right_anchored() -> None:
    assert str(parse_port_address("Jonathans-MacBook/Main Stream/Camera Source 2/Video")) == (
        "jonathans-macbook/main-stream/camera-source-2/video"
    )
    assert str(parse_port_address("Main/Camera/Video")) == "main/camera/video"
    assert str(parse_port_address("Café/Vidéo")) == "cafe/video"


def test_the_address_module_imports_nothing_native() -> None:
    module = ast.parse(PORT_ADDRESS_SOURCE.read_text(encoding="utf-8"))
    imported: "list[tuple[int, str]]" = []
    for statement in ast.walk(module):
        if isinstance(statement, ast.Import):
            imported.extend((0, alias.name) for alias in statement.names)
        elif isinstance(statement, ast.ImportFrom):
            imported.append((statement.level, statement.module or ""))

    assert imported
    for level, module_name in imported:
        if level == 0:
            assert module_name.split(".")[0] in sys.stdlib_module_names, module_name
        else:
            assert (level, module_name) == (1, "_exposed_name_cast"), module_name
