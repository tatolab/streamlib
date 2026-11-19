# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each built-in pin type's Python spelling is the one its contract row lists, and each one the typed grammar reads names that type."""

from __future__ import annotations

import builtins
from collections import Counter

import pytest

import tatolab.stream
from tatolab.stream import NodeDeclarationRefusal
from tatolab.stream._contract_pin_types import BUILT_IN_PIN_TYPES
from tatolab.stream._pin_declaration import read_pin_declaration
from tatolab.stream._pin_types import PIN_TYPE_SPELLINGS, PinTypeSpelling


def test_the_spellings_are_exactly_the_written_forms_of_the_contracts_rows() -> None:
    written_forms_of_the_rows = Counter(
        (written_form, row.name) for row in BUILT_IN_PIN_TYPES for written_form in row.python_forms
    )
    spellings = Counter((spelling.written_form, spelling.pin_type_name) for spelling in PIN_TYPE_SPELLINGS)

    assert spellings == written_forms_of_the_rows


SPELLINGS_THE_TYPED_GRAMMAR_READS = [
    spelling for spelling in PIN_TYPE_SPELLINGS if spelling.not_read_yet_because is None
]

# A value of each type the typed grammar reads, for an input's default.
A_VALUE_OF_EACH_PIN_TYPE: dict[str, object] = {"boolean": False, "integer": 0, "float": 0.0}

# The exact sizes a single pin refuses: each types only a List or Tensor element.
WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES = ("Int64", "UInt64")


@pytest.mark.parametrize(
    "spelling", SPELLINGS_THE_TYPED_GRAMMAR_READS, ids=lambda spelling: spelling.written_form
)
def test_each_spelling_the_typed_grammar_reads_resolves_and_names_its_pin_type(
    spelling: PinTypeSpelling,
) -> None:
    written_type = getattr(builtins, spelling.written_form, None) or getattr(
        tatolab.stream, spelling.written_form
    )
    if spelling.written_form in WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES:
        with pytest.raises(NodeDeclarationRefusal):
            read_pin_declaration("Probe", "value", written_type, default=0, direction="input")
        return

    declared = read_pin_declaration(
        "Probe",
        "value",
        written_type,
        default=A_VALUE_OF_EACH_PIN_TYPE[spelling.pin_type_name],
        direction="input",
    )

    declared_pin_type = declared.declaration["pin_type"]
    assert declared_pin_type["kind"] == "is"
    assert declared_pin_type["pin_type"]["type"] == spelling.pin_type_name
