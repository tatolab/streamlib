# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The record the page reads: the keys `templates/lower_third.html` reads, and a variant it styles."""

from pathlib import Path

import pytest

from lower_third.lower_third_template import (
    DEMO_RUNDOWN,
    LOWER_THIRD_RECORD_NAMES,
    LOWER_THIRD_VARIANTS,
    LowerThirdTemplate,
)
from lower_third.nodes.lower_third_rundown import LowerThirdRundown

LOWER_THIRD_PAGE = Path(__file__).resolve().parents[1] / "templates" / "lower_third.html"


def test_a_lower_third_reaches_the_page_as_the_map_it_reads() -> None:
    lower_third = LowerThirdTemplate(variant="guest", title="A name", subtitle="A role")

    assert lower_third.as_record() == {
        "variant": "guest",
        "title": "A name",
        "subtitle": "A role",
    }


def test_the_rundown_output_declares_every_key_the_page_reads() -> None:
    page = LOWER_THIRD_PAGE.read_text()

    assert LowerThirdRundown.lower_third_to_page.names == LOWER_THIRD_RECORD_NAMES
    for name in LOWER_THIRD_RECORD_NAMES:
        assert f"template.{name}" in page


def test_a_variant_the_page_does_not_style_is_refused() -> None:
    with pytest.raises(ValueError, match="not one of the page's templates"):
        LowerThirdTemplate(variant="weather", title="A", subtitle="B")  # type: ignore[arg-type]


def test_the_page_styles_every_variant_and_the_rundown_uses_more_than_one() -> None:
    page = LOWER_THIRD_PAGE.read_text()

    for variant in LOWER_THIRD_VARIANTS:
        assert f".lower-third.{variant}" in page
    assert len({lower_third.variant for lower_third in DEMO_RUNDOWN}) > 1
