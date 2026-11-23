# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`MakeBundle` and `BreakBundle` take the bundle they carry as their `bundle_type` setting, a
`PinTypeChoice` of a bundle dataclass, and `MakeBundle`'s `members` holds a value of that
bundle, recorded as its stored form."""

from __future__ import annotations

from dataclasses import dataclass

import pytest

from tatolab.stream import (
    BreakBundle,
    FloatToInteger,
    IntegerToFloat,
    MakeBundle,
    PinTypeChoice,
    StreamBuilder,
)


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


def test_a_wire_at_one_member_carries_the_member_on_that_end_of_the_link() -> None:
    stream_builder = StreamBuilder("rig")
    level = stream_builder.add(IntegerToFloat, name="level")
    make = stream_builder.add(MakeBundle, input_values={"bundle_type": PinTypeChoice(Reading)})
    take_apart = stream_builder.add(BreakBundle, input_values={"bundle_type": PinTypeChoice(Reading)})
    rounded = stream_builder.add(FloatToInteger, name="rounded")
    stream_builder.connect(level.output("cast"), make.input("members", member="level"))
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))
    stream_builder.connect(take_apart.output("members", member="level"), rounded.input("value"))

    assert stream_builder._compiled_graph()["links"] == [
        {
            "source": {"node": "level", "port": "cast"},
            "target": {"node": "makebundle", "port": "members", "member": "level"},
        },
        {
            "source": {"node": "makebundle", "port": "bundle"},
            "target": {"node": "breakbundle", "port": "bundle"},
        },
        {
            "source": {"node": "breakbundle", "port": "members", "member": "level"},
            "target": {"node": "rounded", "port": "value"},
        },
    ]


def test_a_member_the_chosen_bundle_does_not_have_is_refused_naming_its_members() -> None:
    make = StreamBuilder("rig").add(
        MakeBundle, input_values={"bundle_type": PinTypeChoice(Reading)}
    )

    with pytest.raises(
        ValueError,
        match=r"`MakeBundle`'s input `members` carries a bundle with no member `gain`; "
        r"its members are: `level`, `label`",
    ):
        make.input("members", member="gain")


def test_a_member_of_a_bundle_no_setting_chooses_is_refused_naming_the_setting() -> None:
    take_apart = StreamBuilder("rig").add(BreakBundle)

    with pytest.raises(
        ValueError,
        match=r"`BreakBundle`'s output `members` carries the bundle `bundle_type` chooses, "
        r"and `bundle_type` is not set",
    ):
        take_apart.output("members", member="level")


def test_a_member_on_a_port_that_carries_no_bundle_is_refused_naming_its_type() -> None:
    cast = StreamBuilder("rig").add(FloatToInteger)

    with pytest.raises(
        TypeError,
        match=r"`FloatToInteger`'s input `value` carries the type Float, not a bundle, so it "
        r"has no member `level`",
    ):
        cast.input("value", member="level")


def test_a_member_on_a_port_whose_chosen_type_is_no_bundle_is_refused_naming_it() -> None:
    take_apart = StreamBuilder("rig").add(
        BreakBundle, input_values={"bundle_type": PinTypeChoice(float)}
    )

    with pytest.raises(TypeError, match=r"`BreakBundle`'s output `members` carries the type Float"):
        take_apart.output("members", member="level")
