# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node whose output is typed by its type parameter, on `tatolabd`: its output states no
format, carries the pictures linked into its input, and they reach the reader as they left the
test pattern.

The declaration and its refusals are tested with no runtime in
`sdk/tatolab-stream/tests/test_class_node_type_parameter.py`.
"""

from collections.abc import Callable

import pytest

from class_node_type_parameter_streams import (
    a_test_pattern_handed_on_by_a_class_typed_by_its_parameter,
)
from conftest import TatolabdUnderTest


@pytest.mark.requires_gpu
def test_a_class_typed_by_its_parameter_hands_on_the_pictures_linked_into_it(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        a_test_pattern_handed_on_by_a_class_typed_by_its_parameter
    )
    pictures = tatolabd.await_marker("PICTURES_HANDED_ON")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(pictures, list), f"no parseable picture report:\n{tatolabd.recent_stderr()}"
    first, second = pictures
    assert (first["width"], first["height"], first["format"]) == (320, 180, "rgba8_unorm"), first
    assert first["surface_id"] != second["surface_id"], "each picture is the pattern's next texture"
    assert second["timestamp_ns"] > first["timestamp_ns"], "the stamps the pattern wrote advance"
