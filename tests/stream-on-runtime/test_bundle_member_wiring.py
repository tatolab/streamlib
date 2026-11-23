# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A stream the Python builder wires at one member of a bundle plays on `tatolabd`: a number
wired into `MakeBundle`'s `level` member, its `label` member set and reached by no wire, the
bundle broken by `BreakBundle`, and a wire out of each of its members into one reporter. A
bundle holding a Texture is made and broken the same way, the test pattern's picture beside a
number.
"""

from __future__ import annotations

from collections.abc import Callable

import pytest

from bundle_member_wiring_nodes import LEVEL_WRITTEN
from bundle_member_wiring_streams import (
    LABEL_SET_ON_MAKE_BUNDLE,
    TEST_PATTERN_HEIGHT,
    TEST_PATTERN_WIDTH,
    a_number_into_one_member_and_each_member_out,
    a_picture_and_a_number_into_members_and_each_member_out,
)
from conftest import TatolabdUnderTest

pytestmark = pytest.mark.requires_gpu

MEMBERS_READ_TIMEOUT_SECONDS = 120.0


def test_a_number_wired_into_one_member_comes_out_of_that_member_on_a_running_stream(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The level read off `BreakBundle`'s `level` member is the one wired into `MakeBundle`'s,
    not the 0.0 set on it, and its `label` member carries the label set on `MakeBundle`.

    Fail-without-fix: a link end written without its `member` is a wire of the whole bundle into
    a Float input, refused at load.
    """
    tatolabd = start_tatolabd_running_stream(a_number_into_one_member_and_each_member_out)
    read = tatolabd.await_marker("BUNDLE_MEMBERS_READ", timeout=MEMBERS_READ_TIMEOUT_SECONDS)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert read == {"level": pytest.approx(LEVEL_WRITTEN), "label": LABEL_SET_ON_MAKE_BUNDLE}, read


def test_a_picture_wired_into_one_member_comes_out_of_that_member_beside_the_number(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The picture read off `BreakBundle`'s `picture` member is the test pattern's, and its
    `level` member carries the level wired into `MakeBundle`, both off one Bundle frame.

    Fail-without-fix: a link into a Texture member is refused at load, and a member is not taken
    from a Bundle frame at the read, so the reporter reads neither.
    """
    tatolabd = start_tatolabd_running_stream(
        a_picture_and_a_number_into_members_and_each_member_out
    )
    read = tatolabd.await_marker(
        "METERED_PICTURE_MEMBERS_READ", timeout=MEMBERS_READ_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert read == {
        "picture": [TEST_PATTERN_WIDTH, TEST_PATTERN_HEIGHT],
        "level": pytest.approx(LEVEL_WRITTEN),
    }, read
