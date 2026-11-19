# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin under bare `@node` states the names it had with `FormerlyNamed`, and a name that would
not find one pin is refused at decoration."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Annotated, Any

import pytest

from tatolab.stream import FormerlyNamed, NodeDeclarationRefusal, NumberUnit, node
from tatolab.stream._function_node_declaration import NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

# What `Hum` declares, which the engine's test of a saved document reads as the described node.
RENAMED_PYTHON_PIN_DECLARATION = (
    Path(__file__).resolve().parents[3]
    / "runtime"
    / "streamlib-engine"
    / "tests"
    / "fixtures"
    / "renamed_python_pin_declaration.json"
)


@node
def Hum(
    frequency: Annotated[float, NumberUnit("Hz"), FormerlyNamed("frequency_hz", "Pitch")] = 440.0,
    detune: float | None = None,
) -> Annotated[float, FormerlyNamed("level")]:
    return frequency + (detune or 0.0)


def test_a_renamed_pin_declares_the_names_it_had_as_port_names_and_an_unset_input_reads_none() -> None:
    declared = json.loads(RENAMED_PYTHON_PIN_DECLARATION.read_text(encoding="utf-8"))

    assert getattr(Hum, NODE_DECLARATION_INPUT_PORTS_STAMP) == declared["input_ports"]
    assert getattr(Hum, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == declared["output_ports"]
    assert getattr(Hum, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP) == declared["function_signature"]
    (frequency, detune) = declared["input_ports"]
    assert frequency["pin_declaration"]["former_names"] == ["frequency_hz", "pitch"]
    assert (detune["pin_default"], detune["pin_declaration"]["may_be_unset"]) == (None, True)
    assert Hum() == 440.0


def test_formerly_named_takes_names_that_each_find_one_pin() -> None:
    assert FormerlyNamed("Frequency Hz").port_names == ("frequency-hz",)
    assert FormerlyNamed("frequency_hz", "pitch").names == ("frequency_hz", "pitch")
    with pytest.raises(ValueError, match="names no pin"):
        FormerlyNamed("!!")
    with pytest.raises(ValueError, match="list each name once"):
        FormerlyNamed("pitch", "Pitch")
    with pytest.raises(TypeError, match="takes each name as a str"):
        FormerlyNamed(3)  # pyright: ignore[reportArgumentType]


def refusal_of(function: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(function)
    return refused.value


# The declarations below are refused; each is undecorated, so the module imports, and at module
# scope, where a function node is declared.


def HumClaimingAnotherPinsName(
    frequency: Annotated[float, FormerlyNamed("gain")] = 440.0, gain: float = 1.0
) -> float:
    return frequency * gain


def HumClaimingItsOwnName(frequency: Annotated[float, FormerlyNamed("Frequency")] = 440.0) -> float:
    return frequency


def HumClaimingAnInputsNameOnItsOutput(
    level: float = 0.0,
) -> Annotated[float, FormerlyNamed("level")]:
    return level


def HumWithTwoPinsClaimingOneName(
    frequency: Annotated[float, FormerlyNamed("pitch")] = 440.0,
    tone: Annotated[float, FormerlyNamed("pitch")] = 0.0,
) -> float:
    return frequency + tone


def HumWithTwoFormerlyNamed(
    frequency: Annotated[float, FormerlyNamed("pitch"), FormerlyNamed("frequency_hz")] = 440.0,
) -> float:
    return frequency


@pytest.mark.parametrize(
    ("function", "pin", "words"),
    [
        pytest.param(
            HumClaimingAnotherPinsName,
            "frequency",
            "and pin `gain` is named that now",
            id="another pin's name",
        ),
        pytest.param(
            HumClaimingItsOwnName,
            "frequency",
            "lists `frequency` as a name it had, and it is named that now",
            id="its own name, once cast",
        ),
        pytest.param(
            HumClaimingAnInputsNameOnItsOutput,
            "result",
            "and pin `level` is named that now",
            id="an input's name on the output",
        ),
        pytest.param(
            HumWithTwoPinsClaimingOneName,
            "tone",
            "pins `frequency` and `tone` both list `pitch`",
            id="two pins listing one name",
        ),
    ],
)
def test_a_former_name_that_would_not_find_one_pin_is_refused_naming_the_pin(
    function: Any, pin: str, words: str
) -> None:
    refusal = refusal_of(function)
    assert refusal.finding_code == "former_name_finds_no_one_pin"
    assert (refusal.node, refusal.pin) == (function.__name__, pin)
    assert words in str(refusal)


def test_formerly_named_written_twice_on_one_pin_is_refused() -> None:
    refusal = refusal_of(HumWithTwoFormerlyNamed)
    assert refusal.finding_code == "pin_annotation_repeated"
    assert "list every name it had in one" in str(refusal)
