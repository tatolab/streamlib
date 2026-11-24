# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class under bare `@node` is built as its helper builds it: every field holding its value and every pin bound before `__init__()` runs."""

from __future__ import annotations

from typing import Annotated, Any

import pytest

from tatolab.stream import Color, SettingOnly, Texture, node
from tatolab.stream._class_node_construction import (
    assign_live_field_values,
    class_node_members,
    construct_class_node,
)
from tatolab.stream._class_node_pins import InputPin, OutputPin
from tatolab.stream._pin_types import Trigger


class StandInPin:
    """A stand-in a test hands a pin, named by the port it stands for."""

    def __init__(self, port: str) -> None:
        self.port = port


@node
class Blur:
    radius: float = 4.0
    tint: Color = Color(1.0, 0.5, 0.25)
    kernel_size: Annotated[int, SettingOnly()] = 3
    labels: list[str] = ["first"]  # noqa: RUF012
    video: InputPin[Texture]
    reset: InputPin[Trigger]
    blurred: OutputPin[Texture]

    def __init__(self) -> None:
        self._seen_at_init: "dict[str, Any]" = {
            "radius": self.radius,
            "tint": self.tint,
            "kernel_size": self.kernel_size,
            "video": self.video,
            "blurred": self.blurred,
        }

    def process(self, ctx: Any) -> None:
        pass


def every_pin_of_blur() -> "dict[str, StandInPin]":
    return {port: StandInPin(port) for port in ("video", "reset", "blurred")}


def test_every_field_holds_its_value_and_every_pin_is_bound_when_init_runs():
    built = construct_class_node(
        Blur, field_values={"radius": 9.0, "kernel_size": 5}, pins=every_pin_of_blur()
    )

    seen_at_init = built._seen_at_init  # pyright: ignore[reportPrivateUsage]
    assert seen_at_init["radius"] == 9.0
    assert seen_at_init["kernel_size"] == 5
    assert seen_at_init["tint"] == Color(1.0, 0.5, 0.25), "a field given no value holds its default"
    assert seen_at_init["video"].port == "video"
    assert seen_at_init["blurred"].port == "blurred"


def test_a_field_left_at_its_default_holds_a_copy_no_other_node_shares():
    first = construct_class_node(Blur, field_values={}, pins=every_pin_of_blur())
    second = construct_class_node(Blur, field_values={}, pins=every_pin_of_blur())

    first.labels.append("only the first")
    assert second.labels == ["first"]
    assert Blur.labels == ["first"]


def test_a_pin_left_unbound_and_a_name_it_does_not_declare_are_refused_by_name():
    with pytest.raises(ValueError, match="no object for its pin `reset`"):
        construct_class_node(
            Blur, field_values={}, pins={"video": StandInPin("video"), "blurred": StandInPin("blurred")}
        )
    with pytest.raises(ValueError, match="declares no field `strength`"):
        construct_class_node(Blur, field_values={"strength": 1.0}, pins=every_pin_of_blur())
    with pytest.raises(ValueError, match="declares no pin `audio`"):
        construct_class_node(
            Blur, field_values={}, pins={**every_pin_of_blur(), "audio": StandInPin("audio")}
        )


def test_what_init_raises_reaches_the_caller():
    @node
    class RefusesAnEvenKernel:
        kernel_size: Annotated[int, SettingOnly()] = 3

        def __init__(self) -> None:
            if self.kernel_size % 2 == 0:
                raise ValueError(f"a kernel of {self.kernel_size} has no centre")

        def process(self, ctx: Any) -> None:
            pass

    with pytest.raises(ValueError, match="a kernel of 4 has no centre"):
        construct_class_node(RefusesAnEvenKernel, field_values={"kernel_size": 4}, pins={})


def test_a_live_value_is_assigned_to_its_field_and_a_setting_is_left_for_its_rebuild():
    built = construct_class_node(Blur, field_values={}, pins=every_pin_of_blur())

    assign_live_field_values(built, {"radius": 2.0, "kernel_size": 7})

    assert built.radius == 2.0
    assert built.kernel_size == 3, "a setting changes only by rebuilding its node"
    with pytest.raises(ValueError, match="declares no field `video`"):
        assign_live_field_values(built, {"video": 1.0})


def test_a_field_named_by_its_cast_port_name_finds_its_attribute():
    @node
    class SpelledApart:
        Gain: float = 1.0

        def process(self, ctx: Any) -> None:
            pass

    members = class_node_members(SpelledApart)
    assert members is not None
    assert [(member.attribute, member.port, member.form) for member in members] == [
        ("Gain", "gain", "field")
    ]
    built = construct_class_node(SpelledApart, field_values={"gain": 3.0}, pins={})
    assert built.Gain == 3.0


def test_only_a_class_under_bare_node_has_members_to_build_it_from():
    class Undecorated:
        def process(self, ctx: Any) -> None:
            pass

    assert class_node_members(Undecorated) is None
    with pytest.raises(TypeError, match="not a class under bare `@node`"):
        construct_class_node(Undecorated, field_values={}, pins={})
