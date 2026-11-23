# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`MakeBundle` and `BreakBundle` take the bundle they carry as their `bundle_type` setting, a
`PinTypeChoice` of a bundle dataclass, and `MakeBundle`'s `members` holds a value of that
bundle, recorded as its stored form."""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from tatolab.stream import BreakBundle, MakeBundle, PinTypeChoice, StreamBuilder


@dataclass(frozen=True)
class Reading:
    level: float
    label: str = "unnamed"


READING = {
    "type": "bundle",
    "members": [
        {"name": "level", "member_type": {"type": "float"}},
        {"name": "label", "member_type": {"type": "string"}, "default_value": "unnamed"},
    ],
}


def test_a_bundle_chosen_and_its_members_set_are_recorded_as_the_runtime_stores_them() -> None:
    stream_builder = StreamBuilder("rig")
    make = stream_builder.add(
        MakeBundle,
        input_values={"bundle_type": PinTypeChoice(Reading), "members": Reading(level=0.5)},
    )
    take_apart = stream_builder.add(BreakBundle, input_values={"bundle_type": PinTypeChoice(Reading)})
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))

    graph = stream_builder._compiled_graph()

    assert [node["input_values"] for node in graph["nodes"]] == [
        {"bundle_type": READING, "members": {"level": 0.5, "label": "unnamed"}},
        {"bundle_type": READING},
    ]
    assert graph["links"] == [
        {
            "source": {"node": "makebundle", "port": "bundle"},
            "target": {"node": "breakbundle", "port": "bundle"},
        }
    ]


def test_members_set_without_a_bundle_chosen_are_recorded_as_given() -> None:
    stream_builder = StreamBuilder("rig")
    stream_builder.add(MakeBundle, input_values={"members": {"level": 0.5}})

    (recorded,) = stream_builder._compiled_graph()["nodes"]

    assert recorded["input_values"] == {"members": {"level": 0.5}}


def test_members_that_are_not_the_bundle_chosen_are_refused_naming_the_input() -> None:
    with pytest.raises(TypeError, match=r"`MakeBundle`'s input `members` is set 0\.5"):
        StreamBuilder("rig").add(
            MakeBundle, input_values={"bundle_type": PinTypeChoice(Reading), "members": 0.5}
        )
