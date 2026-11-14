# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The description lint finds each missing description and each one that writes raw HTML."""

from __future__ import annotations

import sys
import textwrap
from collections.abc import Iterator
from pathlib import Path

import pytest

from tatolab.stream.description_lint import (
    lint_the_descriptions_of_the_node_types_in,
    main,
    writes_raw_html,
)

PACKAGE_NAME = "description_lint_fixture_nodes"

PARTLY_DESCRIBED_NODES_SOURCE = '''
import dataclasses
from typing import Annotated

from tatolab.stream import Description, Image, ImageFrame, Input, Output, node


@dataclasses.dataclass
class BlurConfig:
    radius: Annotated[int, Description("The radius in pixels.")] = 3
    sigma: float = 1.0


@node.meta(description="Blurs each frame.")
@node.process
class Blur:
    """A docstring the lint never counts as a description."""

    video_in = Input(Image, delivery="newest", description="Frames to blur.")
    mask = Input(Image, delivery="newest")
    video = Output(Image, description="<b>Blurred</b> frames.")

    def __init__(self, config: BlurConfig) -> None:
        self.config = config


@node.function
def brighten(
    image: Annotated[ImageFrame, Description("The picture.")], amount: float
) -> ImageFrame:
    return image
'''

FULLY_DESCRIBED_NODES_SOURCE = '''
from typing import Annotated

from tatolab.stream import Description, node


@node.meta(description="Doubles `x`, see <https://tatolab.com>.")
@node.function
def doubled(
    x: Annotated[float, Description("The value, when `x < <b>`.")],
) -> Annotated[float, Description("Twice `x`.")]:
    return 2 * x
'''


@pytest.fixture
def fixture_package(tmp_path: Path) -> Iterator[str]:
    package_directory = tmp_path / PACKAGE_NAME
    package_directory.mkdir()
    (package_directory / "__init__.py").write_text("")
    (package_directory / "blur.py").write_text(textwrap.dedent(PARTLY_DESCRIBED_NODES_SOURCE))
    (package_directory / "doubled.py").write_text(textwrap.dedent(FULLY_DESCRIBED_NODES_SOURCE))
    sys.path.insert(0, str(tmp_path))
    try:
        yield PACKAGE_NAME
    finally:
        sys.path.remove(str(tmp_path))
        for module_name in [name for name in sys.modules if name.startswith(PACKAGE_NAME)]:
            del sys.modules[module_name]


def test_each_missing_description_and_each_raw_html_one_is_found(fixture_package: str):
    findings = [str(finding) for finding in lint_the_descriptions_of_the_node_types_in([fixture_package])]
    blur = f"{fixture_package}.blur:Blur"
    brighten = f"{fixture_package}.blur:brighten"
    assert findings == [
        f"{blur}: input port `mask` has no description",
        f"{blur}: output port `video`'s description writes raw HTML, which a description's "
        f"CommonMark does not allow",
        f"{blur}: config key `sigma` has no description",
        f"{brighten}: the node type has no description",
        f"{brighten}: input port `amount` has no description",
        f"{brighten}: output port `result` has no description",
    ]


def test_a_fully_described_module_has_no_finding(fixture_package: str):
    assert lint_the_descriptions_of_the_node_types_in([f"{fixture_package}.doubled"]) == []


def test_the_command_exits_one_on_a_finding_and_zero_on_none(
    fixture_package: str, capsys: pytest.CaptureFixture[str]
):
    assert main([fixture_package]) == 1
    assert "input port `mask` has no description" in capsys.readouterr().out
    assert main([f"{fixture_package}.doubled"]) == 0


@pytest.mark.parametrize(
    "raw_html",
    ["<b>bold</b>", "line<br/>break", "a <img src=x> image", "<!-- a comment -->", "</div>", "<?php ?>"],
)
def test_raw_html_is_found(raw_html: str):
    assert writes_raw_html(raw_html)


@pytest.mark.parametrize(
    "not_raw_html",
    [
        "see <https://tatolab.com>",
        "mail <someone@example.com>",
        "when `a <b>` holds",
        "x < 3 and y > 2",
        "```\n<div>in a fence</div>\n```",
        "a <- b",
    ],
)
def test_an_autolink_a_comparison_and_code_are_not_raw_html(not_raw_html: str):
    assert not writes_raw_html(not_raw_html)
