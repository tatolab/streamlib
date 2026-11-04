# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The record the page reads: the keys `templates/lower_third.html` reads, and a variant it styles."""

from pathlib import Path

import pytest
from tatolab.stream import msgpack

from lower_third.lower_third_template import (
    DEMO_RUNDOWN,
    LOWER_THIRD_VARIANTS,
    LowerThirdTemplate,
)

LOWER_THIRD_PAGE = Path(__file__).resolve().parents[1] / "templates" / "lower_third.html"


def test_a_lower_third_reaches_the_page_as_the_map_it_reads() -> None:
    lower_third = LowerThirdTemplate(variant="guest", title="A name", subtitle="A role")

    assert msgpack.unpack(lower_third.to_bytes()) == {
        "variant": "guest",
        "title": "A name",
        "subtitle": "A role",
    }
    assert LowerThirdTemplate.from_bytes(lower_third.to_bytes()) == lower_third


def test_a_variant_the_page_does_not_style_is_refused() -> None:
    with pytest.raises(ValueError, match="not one of the page's templates"):
        LowerThirdTemplate(variant="weather", title="A", subtitle="B")  # type: ignore[arg-type]


def test_the_page_styles_every_variant_and_the_rundown_uses_more_than_one() -> None:
    page = LOWER_THIRD_PAGE.read_text()

    for variant in LOWER_THIRD_VARIANTS:
        assert f".lower-third.{variant}" in page
    assert len({lower_third.variant for lower_third in DEMO_RUNDOWN}) > 1
