# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The pure-Python cast against the fixture the engine's cast is held to."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from streamlib._exposed_name_cast import (
    ExposedNameCastsToNothingError,
    cast_exposed_name_to_url_safe,
)

SHARED_EXPOSED_NAME_CAST_CASES = (
    Path(__file__).resolve().parents[3]
    / "runtime"
    / "streamlib-engine"
    / "tests"
    / "fixtures"
    / "exposed_name_cast_cases.json"
)

_cases = json.loads(SHARED_EXPOSED_NAME_CAST_CASES.read_text(encoding="utf-8"))


@pytest.mark.parametrize(
    "case",
    [case for case in _cases if not case.get("refused")],
    ids=lambda case: repr(case["name"])[:40],
)
def test_a_fixture_name_casts_as_the_engine_casts_it(case: "dict[str, str]") -> None:
    assert cast_exposed_name_to_url_safe(case["name"]) == case["cast"]


@pytest.mark.parametrize(
    "case",
    [case for case in _cases if case.get("refused")],
    ids=lambda case: repr(case["name"]),
)
def test_a_fixture_name_casting_to_nothing_is_refused_by_name(
    case: "dict[str, str]",
) -> None:
    with pytest.raises(ExposedNameCastsToNothingError, match="cannot name anything"):
        cast_exposed_name_to_url_safe(case["name"])
