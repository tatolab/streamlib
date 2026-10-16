# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_display_window.py` runs."""

from tatolab.stream import DisplayWindow, StreamBuilder, TestPatternSource, stream

PATTERN_NODE_NAME = "pattern"
WINDOW_NODE_NAME = "window"
WINDOW_TITLE = "a window on the software device"


@stream
def a_test_pattern_into_a_display_window(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name=PATTERN_NODE_NAME, config={"width": 320, "height": 180}
    )
    window = stream_builder.add(
        DisplayWindow, name=WINDOW_NODE_NAME, config={"title": WINDOW_TITLE}
    )
    stream_builder.connect(pattern.output("video"), window.input("video"))
