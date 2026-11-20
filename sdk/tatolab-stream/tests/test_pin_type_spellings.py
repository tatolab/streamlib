# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each built-in pin type's Python spelling is the one its contract row lists, and each one the typed grammar reads names that type."""

from __future__ import annotations

import builtins
from collections import Counter
from collections.abc import Buffer
from typing import Literal, Self

import pytest

import tatolab.stream
from tatolab.stream import NodeDeclarationRefusal
from tatolab.stream._contract_pin_types import BUILT_IN_PIN_TYPES
from tatolab.stream._pin_declaration import NO_DEFAULT, read_pin_declaration
from tatolab.stream._pin_types import PIN_TYPE_SPELLINGS, PIN_VALUE_CLASSES, PinTypeSpelling


def test_the_spellings_are_exactly_the_written_forms_of_the_contracts_rows() -> None:
    written_forms_of_the_rows = Counter(
        (written_form, row.name) for row in BUILT_IN_PIN_TYPES for written_form in row.python_forms
    )
    spellings = Counter((spelling.written_form, spelling.pin_type_name) for spelling in PIN_TYPE_SPELLINGS)

    assert spellings == written_forms_of_the_rows


@tatolab.stream.named_pin_type(description="A probe's reading, packed as one byte.")
class ProbeReading:
    def __init__(self, level: int) -> None:
        self.level = level

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        return cls(bytes(payload)[0])

    def to_bytes(self) -> bytes:
        return bytes([self.level])


SPELLINGS_THE_TYPED_GRAMMAR_READS = [
    spelling
    for spelling in PIN_TYPE_SPELLINGS
    if spelling.not_read_yet_because is None and not spelling.only_a_pin_carries
]

SPELLINGS_ONLY_A_PIN_CARRIES = [spelling for spelling in PIN_TYPE_SPELLINGS if spelling.only_a_pin_carries]

# A value of each spelling the typed grammar reads, for an input's default.
A_VALUE_OF_EACH_SPELLING: dict[str, object] = {
    "bool": False,
    "int": 0,
    "Int8": 0,
    "Int16": 0,
    "Int32": 0,
    "Int64": 0,
    "UInt8": 0,
    "UInt16": 0,
    "UInt32": 0,
    "UInt64": 0,
    "float": 0.0,
    "Float32": 0.0,
    "Float64": 0.0,
    "Color": tatolab.stream.Color(0.0, 0.0, 0.0),
    "Vector2": tatolab.stream.Vector2(0.0, 0.0),
    "Vector3": tatolab.stream.Vector3(0.0, 0.0, 0.0),
    "Vector4": tatolab.stream.Vector4(0.0, 0.0, 0.0, 0.0),
    "Rectangle": tatolab.stream.Rectangle(0.0, 0.0, 1.0, 1.0),
    "Transform": tatolab.stream.Transform.identity(),
    "str": "",
    "Literal": "sine",
    "Asset": tatolab.stream.Asset("0eaf9ee2-0f73-4154-a3f6-a99c261b850d"),
    "list": [0.25, 0.5],
    "named_pin_type": NO_DEFAULT,
}

# The type written for each spelling that names a family of types rather than one.
A_TYPE_OF_EACH_GENERIC_SPELLING: dict[str, object] = {
    "Literal": Literal["sine", "square"],
    "list": list[float],
    "named_pin_type": ProbeReading,
}

# The exact sizes a single pin refuses: each types only a List or Tensor element.
WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES = ("Int64", "UInt64")


@pytest.mark.parametrize(
    "spelling", SPELLINGS_THE_TYPED_GRAMMAR_READS, ids=lambda spelling: spelling.written_form
)
def test_each_spelling_the_typed_grammar_reads_resolves_and_names_its_pin_type(
    spelling: PinTypeSpelling,
) -> None:
    written_type = (
        A_TYPE_OF_EACH_GENERIC_SPELLING.get(spelling.written_form)
        or getattr(builtins, spelling.written_form, None)
        or getattr(tatolab.stream, spelling.written_form)
    )
    if spelling.written_form in WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES:
        with pytest.raises(NodeDeclarationRefusal):
            read_pin_declaration(
                "Probe",
                "value",
                written_type,
                default=A_VALUE_OF_EACH_SPELLING[spelling.written_form],
                direction="input",
            )
        return

    declared = read_pin_declaration(
        "Probe",
        "value",
        written_type,
        default=A_VALUE_OF_EACH_SPELLING[spelling.written_form],
        direction="input",
    )

    declared_pin_type = declared.declaration["pin_type"]
    assert declared_pin_type["kind"] == "is"
    assert declared_pin_type["pin_type"]["type"] == spelling.pin_type_name


def test_every_spelling_the_typed_grammar_reads_has_a_value_here() -> None:
    assert sorted(A_VALUE_OF_EACH_SPELLING) == sorted(
        spelling.written_form for spelling in SPELLINGS_THE_TYPED_GRAMMAR_READS
    )


@pytest.mark.parametrize(
    "spelling", SPELLINGS_ONLY_A_PIN_CARRIES, ids=lambda spelling: spelling.written_form
)
def test_each_spelling_only_a_pin_carries_names_its_pin_type_on_a_pin_and_is_refused_as_a_value(
    spelling: PinTypeSpelling,
) -> None:
    written_type = getattr(tatolab.stream, spelling.written_form)

    declared = read_pin_declaration("Probe", "value", written_type, direction="input", form="pin")
    with pytest.raises(NodeDeclarationRefusal) as as_a_field:
        read_pin_declaration("Probe", "value", written_type, direction="input", form="field")
    with pytest.raises(NodeDeclarationRefusal) as as_a_parameter:
        read_pin_declaration("Probe", "value", written_type, direction="input")

    declared_pin_type = declared.declaration["pin_type"]
    assert declared_pin_type["kind"] == "is"
    assert declared_pin_type["pin_type"]["type"] == spelling.pin_type_name
    assert as_a_field.value.finding_code == "field_typed_as_a_stream"
    assert f"InputPin[{spelling.written_form}]" in str(as_a_field.value)
    assert as_a_parameter.value.finding_code == "pin_type_not_read_yet"


def test_each_value_class_declares_the_pin_type_its_contract_row_names() -> None:
    for value_class in PIN_VALUE_CLASSES:
        spelling = next(
            spelling for spelling in PIN_TYPE_SPELLINGS if spelling.written_form == value_class.__name__
        )
        assert value_class.pin_type["type"] == spelling.pin_type_name
