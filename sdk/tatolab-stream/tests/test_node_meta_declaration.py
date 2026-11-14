# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`@node.meta` declares a node type's category and description, above or below the decorator that says its kind."""

from __future__ import annotations

from typing import Any

import pytest
from tatolab.stream import Image, ImageFrame, Input, node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CATEGORY_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
)
from tatolab.stream._node_meta_declaration import (
    NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE,
)

HALFTONE_DESCRIPTION = "Turns what a drifting lens covers into dots."


def _slang_passing_the_image_on(function_name: str) -> str:
    return f"float4 {function_name}(ImageInput image) {{ return image.pixel; }}"


@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
@node.process
class HalftoneWithItsMetaAbove:
    video = Input(Image, delivery="newest")


@node.process
@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
class HalftoneWithItsMetaBelow:
    video = Input(Image, delivery="newest")


@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
@node.function
def halftone_level_with_its_meta_above(level: float) -> float:
    return level


@node.function
@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
def halftone_level_with_its_meta_below(level: float) -> float:
    return level


@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
@node.shader(_slang_passing_the_image_on("halftone_shader_with_its_meta_above"))
def halftone_shader_with_its_meta_above(image: ImageFrame) -> ImageFrame: ...


@node.shader(_slang_passing_the_image_on("halftone_shader_with_its_meta_below"))
@node.meta(category="Effects/Stylize", description=HALFTONE_DESCRIPTION)
def halftone_shader_with_its_meta_below(image: ImageFrame) -> ImageFrame: ...


@pytest.mark.parametrize(
    "declared",
    [
        HalftoneWithItsMetaAbove,
        HalftoneWithItsMetaBelow,
        halftone_level_with_its_meta_above,
        halftone_level_with_its_meta_below,
        halftone_shader_with_its_meta_above,
        halftone_shader_with_its_meta_below,
    ],
)
def test_every_kind_carries_its_meta_whether_written_above_or_below_its_kind_decorator(
    declared: Any,
):
    assert getattr(declared, NODE_DECLARATION_CATEGORY_STAMP) == "Effects/Stylize"
    assert getattr(declared, NODE_DECLARATION_DESCRIPTION_STAMP) == HALFTONE_DESCRIPTION
    assert not hasattr(declared, NODE_META_HELD_FOR_THE_KIND_DECORATOR_ATTRIBUTE)


def test_a_node_that_declares_only_a_description_has_no_category():
    @node.meta(description="Passes frames on.")
    @node.process
    class DescribedOnly:
        video = Input(Image, delivery="newest")

    assert getattr(DescribedOnly, NODE_DECLARATION_CATEGORY_STAMP) is None
    assert getattr(DescribedOnly, NODE_DECLARATION_DESCRIPTION_STAMP) == "Passes frames on."


def test_a_node_that_declares_only_a_category_is_described_by_the_empty_string():
    @node.process
    @node.meta(category="Effects")
    class CategorizedOnly:
        video = Input(Image, delivery="newest")

    assert getattr(CategorizedOnly, NODE_DECLARATION_CATEGORY_STAMP) == "Effects"
    assert getattr(CategorizedOnly, NODE_DECLARATION_DESCRIPTION_STAMP) == ""


def test_two_metas_on_one_node_join_and_the_one_written_above_wins_a_key_both_declare():
    @node.meta(category="Effects/Stylize")
    @node.process
    @node.meta(description="Dots.")
    @node.meta(category="Effects")
    class DeclaredInThreeMetas:
        video = Input(Image, delivery="newest")

    assert getattr(DeclaredInThreeMetas, NODE_DECLARATION_CATEGORY_STAMP) == "Effects/Stylize"
    assert getattr(DeclaredInThreeMetas, NODE_DECLARATION_DESCRIPTION_STAMP) == "Dots."


def test_a_class_never_takes_the_meta_of_the_class_it_extends():
    @node.meta(category="Effects")
    class NotYetANode:
        video = Input(Image, delivery="newest")

    @node.process
    class ExtendsIt(NotYetANode):
        pass

    assert getattr(ExtendsIt, NODE_DECLARATION_CATEGORY_STAMP) is None


def test_a_key_meta_does_not_take_is_refused_naming_it():
    with pytest.raises(TypeError, match="colour"):
        node.meta(colour="red")  # type: ignore[call-arg]


def test_meta_with_no_key_is_refused():
    with pytest.raises(TypeError, match="declares nothing"):
        node.meta()


@pytest.mark.parametrize("not_a_path", ["", "  ", "Effects/", "/Stylize", "Effects//Stylize"])
def test_a_category_with_a_level_that_has_no_name_is_refused(not_a_path: str):
    with pytest.raises(ValueError, match="names a group with no name"):
        node.meta(category=not_a_path)


def test_a_category_that_is_not_a_string_is_refused():
    with pytest.raises(TypeError, match="takes its category as a str"):
        node.meta(category=7)  # type: ignore[arg-type]


@pytest.mark.parametrize("meta_key", ["description", "category"])
def test_a_kind_decorator_refuses_a_meta_key_naming_it(meta_key: str):
    with pytest.raises(TypeError, match=meta_key):
        node.process(**{meta_key: "Effects"})  # type: ignore[arg-type]
    with pytest.raises(TypeError, match=meta_key):
        node.function(**{meta_key: "Effects"})  # type: ignore[call-overload]
    with pytest.raises(TypeError, match=meta_key):
        node.shader(_slang_passing_the_image_on("tint"), **{meta_key: "Effects"})  # type: ignore[call-arg]
