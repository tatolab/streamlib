# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Texture output states the frame rate its stream runs at, fixed or read from one of its
node's value inputs, and a frame rate anywhere else is refused with what to write."""

from __future__ import annotations

from typing import Annotated, Any

import pytest

from tatolab.stream import (
    FrameRate,
    FrameRateFromSetting,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    Texture,
    node,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_OUTPUT_PORTS_STAMP


def output_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_OUTPUT_PORTS_STAMP) if port["name"] == name)


def refusal_of(node_function: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(execution="reactive")(node_function)
    return refused.value


def test_a_texture_output_states_a_fixed_frame_rate_or_one_a_setting_holds() -> None:
    @node(execution="reactive")
    def Camera(
        *,
        frames_per_second: Input[int] = Input(30),
        video: OutputPin[Annotated[Texture, FrameRate(30000, 1001)]],
        preview: OutputPin[Annotated[Texture, FrameRateFromSetting("frames_per_second")]],
        still: OutputPin[Texture],
    ) -> None:
        return None

    assert output_port(Camera, "video")["pin_declaration"]["annotations"] == {
        "frame_rate": {"stated": {"numerator": 30000, "denominator": 1001}}
    }
    assert output_port(Camera, "preview")["pin_declaration"]["annotations"] == {
        "frame_rate": {"from_config": {"config_key": "frames_per_second"}}
    }
    assert "annotations" not in output_port(Camera, "still")["pin_declaration"]


def FrameRateOnAnInput(*, video: InputPin[Annotated[Texture, FrameRate(30)]]) -> None: ...


def FrameRateOnAFloat(*, level: OutputPin[Annotated[float, FrameRateFromSetting("rate")]]) -> None: ...


def TwoFrameRates(
    *, video: OutputPin[Annotated[Texture, FrameRate(30), FrameRateFromSetting("rate")]]
) -> None: ...


@pytest.mark.parametrize(
    ("node_function", "finding_code", "fix"),
    [
        (FrameRateOnAnInput, "pin_annotation_not_taken", "state it on the Texture output that sends the frames"),
        (FrameRateOnAFloat, "pin_annotation_not_taken", "`OutputPin[Annotated[Texture, FrameRateFromSetting(...)]]`"),
        (TwoFrameRates, "pin_annotation_repeated", "keep one"),
    ],
)
def test_a_frame_rate_anywhere_but_on_one_texture_output_is_refused_with_what_to_write(
    node_function: Any, finding_code: str, fix: str
) -> None:
    refusal = refusal_of(node_function)

    assert refusal.finding_code == finding_code
    assert fix in str(refusal)


@pytest.mark.parametrize(
    ("arguments", "refused_as"),
    [((0,), ValueError), ((30, 0), ValueError), ((2**32,), ValueError), ((True,), TypeError), ((29.97,), TypeError)],
)
def test_a_frame_rate_is_a_whole_number_of_frames_over_a_whole_number_of_seconds(
    arguments: tuple[object, ...], refused_as: type[Exception]
) -> None:
    with pytest.raises(refused_as):
        FrameRate(*arguments)  # pyright: ignore[reportArgumentType]


def test_a_frame_rate_from_a_setting_names_the_setting() -> None:
    with pytest.raises(ValueError):
        FrameRateFromSetting(" ")
