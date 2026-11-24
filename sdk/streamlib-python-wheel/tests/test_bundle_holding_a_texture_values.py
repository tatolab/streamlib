# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A bundle holding a Texture, as a class node's pin builds it from the members a Bundle frame
read hands back, and splits the dataclass a node gives into its small members' stored forms and
its Textures."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.runtime._bundle_holding_a_texture import (
    holds_a_texture,
    the_bundle_a_frame_read_builds,
    the_small_members_and_textures_of,
)
from tatolab.stream import Rectangle

FRAMED: "dict[str, Any]" = {
    "type": "bundle",
    "members": [
        {"name": "picture", "member_type": {"type": "texture"}},
        {
            "name": "region",
            "member_type": {"type": "rectangle"},
            "default_value": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0},
        },
    ],
}


@dataclass(frozen=True)
class APicture:
    """Stands in for the `Texture` a read hands back: what names its pool slot."""

    surface_id: str


@dataclass(frozen=True)
class Framed:
    picture: Any
    region: Rectangle = Rectangle(0.0, 0.0, 1.0, 1.0)


def test_only_a_bundle_holding_a_texture_travels_as_a_bundle_frame() -> None:
    assert holds_a_texture(FRAMED)
    assert not holds_a_texture({"type": "bundle", "members": FRAMED["members"][1:]})
    assert not holds_a_texture({"type": "texture"})


def test_the_bundle_is_built_with_the_picture_as_it_was_read_and_each_small_member_in_its_python_form() -> None:
    picture = APicture("surface-7")
    built = the_bundle_a_frame_read_builds(
        FRAMED,
        {"picture": picture, "region": {"x": 0.25, "y": 0.5, "width": 0.5, "height": 0.25}},
        "`Crop`'s input `framed`",
        Framed,
    )

    assert built == Framed(picture, Rectangle(0.25, 0.5, 0.5, 0.25))
    assert built.picture is picture, "the Texture reaches the node as the read handed it, no copy"


def test_a_given_bundle_splits_into_its_small_members_stored_forms_and_its_textures() -> None:
    picture = APicture("surface-7")
    small_members, textures = the_small_members_and_textures_of(
        FRAMED, Framed(picture, Rectangle(0.25, 0.5, 0.5, 0.25)), "output `cropped`"
    )

    assert small_members == {"region": {"x": 0.25, "y": 0.5, "width": 0.5, "height": 0.25}}
    assert textures == {"picture": picture}


@dataclass(frozen=True)
class OnlyARegion:
    region: Rectangle


def test_a_given_bundle_that_is_no_dataclass_of_its_members_or_holds_no_texture_is_refused() -> None:
    with pytest.raises(TypeError, match="is a bundle of `picture`, `region`, and was given dict"):
        the_small_members_and_textures_of(FRAMED, {"picture": None}, "output `cropped`")
    with pytest.raises(ValueError, match="was given a `OnlyARegion` of `region`"):
        the_small_members_and_textures_of(
            FRAMED, OnlyARegion(Rectangle(0.0, 0.0, 1.0, 1.0)), "output `cropped`"
        )
    with pytest.raises(TypeError, match="member `picture` is a Texture.*was given bytes"):
        the_small_members_and_textures_of(FRAMED, Framed(b"pixels"), "output `cropped`")
