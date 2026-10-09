# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# pyright: reportUnnecessaryTypeIgnoreComment=true

"""A stream exposes an output private or public, the level an `Exposure` member and never a string.

Pyright reports an ignore comment that silences nothing in this file, so the
ignore on the string call below is the proof that pyright refuses a string level.
"""

import pytest
from stream_graph_builder_nodes import FrameInverter
from tatolab.stream import Exposure, StreamBuilder, compile_stream_to_graph, stream


@stream
def a_stream_exposing_one_output_private_and_one_public(stream_builder: StreamBuilder) -> None:
    on_this_machine = stream_builder.add(FrameInverter, name="on-this-machine")
    off_this_machine = stream_builder.add(FrameInverter, name="off-this-machine")
    stream_builder.expose(on_this_machine.output("video_to_downstream"))
    stream_builder.expose(off_this_machine.output("video_to_downstream"), Exposure.PUBLIC)


def test_expose_makes_an_output_private_and_exposure_public_makes_it_public() -> None:
    graph = compile_stream_to_graph(a_stream_exposing_one_output_private_and_one_public)

    assert graph["exposed"] == [
        {"node": "on-this-machine", "port": "video_to_downstream", "level": "private"},
        {"node": "off-this-machine", "port": "video_to_downstream", "level": "public"},
    ]


def test_a_level_spelled_as_a_string_is_refused_naming_the_enum() -> None:
    stream_builder = StreamBuilder("rig")
    inverter = stream_builder.add(FrameInverter)

    with pytest.raises(TypeError, match=r"`Exposure`") as refusal:
        stream_builder.expose(
            inverter.output("video_to_downstream"),
            "public",  # pyright: ignore[reportArgumentType]
        )

    assert "`Exposure.PUBLIC`" in str(refusal.value)


def test_the_enum_offers_exactly_private_and_public() -> None:
    assert [(member.name, member.value) for member in Exposure] == [
        ("PRIVATE", "private"),
        ("PUBLIC", "public"),
    ]
