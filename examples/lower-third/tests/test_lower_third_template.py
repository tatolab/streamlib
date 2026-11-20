# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The record the page reads: the keys `templates/lower_third.html` reads, and a variant it styles."""

from pathlib import Path
from typing import Any

import pytest
from tatolab.browser import PageMessage
from tatolab.stream._class_node_construction import construct_class_node

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


class MessagesWritten:
    """Stands in for the rundown's output pin, holding each message written on it."""

    def __init__(self) -> None:
        self.messages: list[Any] = []

    def write(self, value: Any) -> None:
        self.messages.append(value)


def test_each_rundown_message_carries_every_key_the_page_reads() -> None:
    page = LOWER_THIRD_PAGE.read_text()
    written = MessagesWritten()
    rundown = construct_class_node(
        LowerThirdRundown, field_values={}, pins={"lower_third_to_page": written}
    )

    rundown.process(None)  # type: ignore[arg-type]

    [message] = written.messages
    assert isinstance(message, PageMessage)
    assert tuple(message.members) == LOWER_THIRD_RECORD_NAMES
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
