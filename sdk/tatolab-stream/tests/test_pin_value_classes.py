# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Color, a Vector, a Rectangle, a Transform and an Asset: their fields are their stored form's keys, in the order they pack."""

from __future__ import annotations

import math

import pytest

from tatolab.stream import Asset, Color, Rectangle, Transform, Vector2, Vector3, Vector4
from tatolab.stream._pin_types import PIN_VALUE_CLASSES, PinValueOfParts


def test_a_colour_is_opaque_unless_it_says_otherwise_and_holds_its_numbers_as_floats() -> None:
    colour = Color(1, 0.5, 0.2)
    assert colour.parts == (1.0, 0.5, 0.2, 1.0)
    assert isinstance(colour.red, float)
    assert colour.stored_value() == {"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0}


def test_a_colour_number_outside_0_to_1_is_kept_as_given() -> None:
    assert Color(2.0, -0.5, 0.0, 1.0).parts == (2.0, -0.5, 0.0, 1.0)


@pytest.mark.parametrize(
    ("value", "stored_value"),
    [
        (Vector2(0.5, 0.25), {"x": 0.5, "y": 0.25}),
        (Vector3(1, 2, 3), {"x": 1.0, "y": 2.0, "z": 3.0}),
        (Vector4(1, 2, 3, 4), {"x": 1.0, "y": 2.0, "z": 3.0, "w": 4.0}),
        (Rectangle(80, 800, 640, 360), {"x": 80.0, "y": 800.0, "width": 640.0, "height": 360.0}),
        (Color(0.1, 0.2, 0.3, 0.4), {"red": 0.1, "green": 0.2, "blue": 0.3, "alpha": 0.4}),
    ],
)
def test_each_value_is_its_stored_form_keyed_by_field_and_reads_back_from_it(
    value: PinValueOfParts, stored_value: dict[str, float]
) -> None:
    assert value.stored_value() == stored_value
    assert type(value).from_stored_value(stored_value) == value
    assert type(value).from_parts(value.parts) == value
    assert type(value).part_names() == tuple(stored_value)


def test_a_transform_is_16_numbers_row_by_row_and_its_stored_form_is_their_list() -> None:
    identity = Transform.identity()
    assert identity.stored_value() == [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]
    moved = Transform((1, 0, 0, 5, 0, 1, 0, 6, 0, 0, 1, 7, 0, 0, 0, 1))
    assert moved.parts[3] == 5.0
    assert Transform.from_stored_value(moved.stored_value()) == moved
    assert len(Transform.part_names()) == 16
    with pytest.raises(ValueError, match="16 numbers"):
        Transform((1.0, 0.0))


@pytest.mark.parametrize(
    "stored_value",
    [
        {"red": 1.0, "green": 0.5, "blue": 0.2},
        {"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0, "hue": 3.0},
        [1.0, 0.5, 0.2, 1.0],
    ],
)
def test_a_stored_value_missing_a_key_or_naming_another_is_refused(stored_value: object) -> None:
    with pytest.raises(ValueError, match="exactly red, green, blue, alpha"):
        Color.from_stored_value(stored_value)


def test_a_vector_of_another_size_is_refused() -> None:
    with pytest.raises(ValueError, match="3 numbers"):
        Vector3.from_parts((1.0, 2.0))
    with pytest.raises(ValueError):
        Vector2.from_stored_value({"x": 1.0, "y": 2.0, "z": 3.0})


def test_a_rectangle_of_a_negative_width_or_height_is_refused() -> None:
    with pytest.raises(ValueError, match="width is 0 or more"):
        Rectangle(0, 0, -1, 1)
    with pytest.raises(ValueError, match="height is 0 or more"):
        Rectangle(0, 0, 1, -0.5)
    assert Rectangle(0, 0, 0, 0).parts == (0.0, 0.0, 0.0, 0.0)


@pytest.mark.parametrize("not_a_number", [True, "1", None, math.nan, math.inf])
def test_a_part_that_is_no_finite_number_is_refused(not_a_number: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        Color(not_a_number, 0.0, 0.0)  # pyright: ignore[reportArgumentType]


def test_a_value_is_frozen() -> None:
    colour = Color(1.0, 1.0, 1.0)
    with pytest.raises(AttributeError):
        colour.red = 0.0  # pyright: ignore[reportAttributeAccessIssue]


def test_each_value_class_has_its_own_value_type() -> None:
    value_types = [value_class.value_type for value_class in PIN_VALUE_CLASSES]
    assert value_types == ["color", "vector2", "vector3", "vector4", "rectangle", "transform"]


ASSET_UUID = "0eaf9ee2-0f73-4154-a3f6-a99c261b850d"
PROJECT_UUID = "5f0c3b1e-27a4-4bd3-9b51-6f0d5bd0b7a2"


def test_an_asset_is_stored_as_the_object_a_saved_document_holds() -> None:
    assert Asset(ASSET_UUID).stored_value() == {"asset": ASSET_UUID}
    assert Asset(ASSET_UUID, PROJECT_UUID).stored_value() == {"asset": ASSET_UUID, "from": PROJECT_UUID}
    assert Asset.from_stored_value({"asset": ASSET_UUID, "from": PROJECT_UUID}) == Asset(
        ASSET_UUID, PROJECT_UUID
    )


def test_an_asset_holds_its_uuid_in_the_lowercase_hyphenated_form() -> None:
    assert Asset(ASSET_UUID.upper()).asset == ASSET_UUID


@pytest.mark.parametrize(
    "stored_value",
    [ASSET_UUID, {"asset": ASSET_UUID, "kind": "image"}, {"from": PROJECT_UUID}, {"asset": "not a uuid"}],
)
def test_an_asset_is_built_only_from_exactly_its_stored_form(stored_value: object) -> None:
    with pytest.raises((TypeError, ValueError)):
        Asset.from_stored_value(stored_value)
