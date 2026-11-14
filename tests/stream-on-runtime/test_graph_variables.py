# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A graph variable on a running stream: an Image set once and read twice with no wire drawn."""

import pytest

import graph_variable_streams

pytestmark = pytest.mark.requires_gpu


def test_an_image_variable_read_twice_reaches_both_readers(
    start_tatolabd_running_stream,
):
    tatolabd = start_tatolabd_running_stream(
        graph_variable_streams.a_test_pattern_held_in_a_variable_and_read_twice
    )
    reads = tatolabd.await_every_marker(
        "GRAPH_VARIABLE_READ_FRONT", "GRAPH_VARIABLE_READ_BACK"
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    front, back = reads["GRAPH_VARIABLE_READ_FRONT"], reads["GRAPH_VARIABLE_READ_BACK"]
    assert front["width"] == back["width"] == graph_variable_streams.PATTERN_WIDTH
    assert front["pid"] != back["pid"], "each reader runs in a process of its own"
