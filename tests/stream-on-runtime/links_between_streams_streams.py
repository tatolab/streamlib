# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_links_between_streams_on_one_machine.py` runs from the suite
project.

Each is loaded under the short name its test gives with `--name`: the source
stream as `a`, the reading stream as `b`, so a reader names a port `a/<node>/<port>`.
"""

from tatolab.stream import DisplayWindow, StreamBuilder, TestPatternSource, stream

from links_between_streams_probes import CountsEachBagItReads, RelaysEachFrameDownstream

#: The name the source stream is loaded under.
SOURCE_STREAM_NAME = "a"
#: The name the reading stream is loaded under.
READER_STREAM_NAME = "b"

PATTERN_NODE = "pattern"
SECOND_PATTERN_NODE = "secondpattern"
RELAY_NODE = "relay"
PATTERN_COUNTER_NODE = "patterncounter"
SECOND_PATTERN_COUNTER_NODE = "secondpatterncounter"
WINDOW_NODE = "window"

PATTERN_PORT = "video"
RELAY_PORT = "video_to_downstream"
COUNTER_PORT = "bags_from_upstream"
WINDOW_PORT = "video"

PATTERN_CONFIG = {"width": 320, "height": 180}
WINDOW_CONFIG = {"width": 160, "height": 90, "title": "links between streams"}

#: A machine no test machine is named.
ANOTHER_MACHINE = "elsewhere"

#: Addresses the builder refuses at the call, each with the words its refusal carries.
ADDRESS_WITH_A_DOT_CHUNK = f"{SOURCE_STREAM_NAME}/./{PATTERN_PORT}"
ADDRESS_OF_ONE_CHUNK = PATTERN_PORT
ADDRESS_OF_FIVE_CHUNKS = "m/s/n/p/x"


def _add_a_pattern(stream_builder: StreamBuilder, name: str = PATTERN_NODE):
    return stream_builder.add(TestPatternSource, name=name, config=PATTERN_CONFIG)


def _add_a_counter(stream_builder: StreamBuilder, name: str = PATTERN_COUNTER_NODE):
    return stream_builder.add(CountsEachBagItReads, name=name, config={"counter": name})


@stream
def a_pattern_and_a_python_relay_each_exposed_private(stream_builder: StreamBuilder) -> None:
    """A native pattern relayed by a Python node; both outputs readable on this machine."""
    pattern = _add_a_pattern(stream_builder)
    relay = stream_builder.add(RelaysEachFrameDownstream, name=RELAY_NODE)
    stream_builder.connect(pattern.output(PATTERN_PORT), relay.input("video_from_upstream"))
    stream_builder.expose(pattern.output(PATTERN_PORT))
    stream_builder.expose(relay.output(RELAY_PORT))


@stream
def b_reading_the_pattern_into_a_python_counter_and_the_relay_into_a_window(
    stream_builder: StreamBuilder,
) -> None:
    """`a`'s native pattern into a Python counter, and `a`'s Python relay into a native window."""
    counter = _add_a_counter(stream_builder)
    window = stream_builder.add(DisplayWindow, name=WINDOW_NODE, config=WINDOW_CONFIG)
    stream_builder.connect(
        stream_builder.remote_output(f"{SOURCE_STREAM_NAME}/{PATTERN_NODE}/{PATTERN_PORT}"),
        counter.input(COUNTER_PORT),
    )
    stream_builder.connect(
        stream_builder.remote_output(f"{SOURCE_STREAM_NAME}/{RELAY_NODE}/{RELAY_PORT}"),
        window.input(WINDOW_PORT),
    )


@stream
def a_pattern_left_internal(stream_builder: StreamBuilder) -> None:
    """A pattern its stream does not expose."""
    _add_a_pattern(stream_builder)


@stream
def a_pattern_exposed_private(stream_builder: StreamBuilder) -> None:
    """A pattern readable by this machine's other streams."""
    pattern = _add_a_pattern(stream_builder)
    stream_builder.expose(pattern.output(PATTERN_PORT))


@stream
def a_two_patterns_each_exposed_private(stream_builder: StreamBuilder) -> None:
    """Two patterns, each readable by this machine's other streams."""
    for pattern_node in (PATTERN_NODE, SECOND_PATTERN_NODE):
        pattern = _add_a_pattern(stream_builder, pattern_node)
        stream_builder.expose(pattern.output(PATTERN_PORT))


@stream
def b_reading_the_pattern_into_a_python_counter(stream_builder: StreamBuilder) -> None:
    """`a`'s pattern into a Python counter."""
    counter = _add_a_counter(stream_builder)
    stream_builder.connect(
        stream_builder.remote_output(f"{SOURCE_STREAM_NAME}/{PATTERN_NODE}/{PATTERN_PORT}"),
        counter.input(COUNTER_PORT),
    )


@stream
def b_reading_both_patterns_into_two_python_counters(stream_builder: StreamBuilder) -> None:
    """Each of `a`'s two patterns into a Python counter of its own."""
    for pattern_node, counter_node in (
        (PATTERN_NODE, PATTERN_COUNTER_NODE),
        (SECOND_PATTERN_NODE, SECOND_PATTERN_COUNTER_NODE),
    ):
        counter = _add_a_counter(stream_builder, counter_node)
        stream_builder.connect(
            stream_builder.remote_output(f"{SOURCE_STREAM_NAME}/{pattern_node}/{PATTERN_PORT}"),
            counter.input(COUNTER_PORT),
        )


@stream
def b_reading_a_pattern_on_another_machine(stream_builder: StreamBuilder) -> None:
    """A link whose source names a machine other than this one."""
    counter = _add_a_counter(stream_builder)
    stream_builder.connect(
        stream_builder.remote_output(
            f"{ANOTHER_MACHINE}/{SOURCE_STREAM_NAME}/{PATTERN_NODE}/{PATTERN_PORT}"
        ),
        counter.input(COUNTER_PORT),
    )


@stream
def b_naming_an_address_with_a_dot_chunk(stream_builder: StreamBuilder) -> None:
    stream_builder.remote_output(ADDRESS_WITH_A_DOT_CHUNK)


@stream
def b_naming_an_address_of_one_chunk(stream_builder: StreamBuilder) -> None:
    stream_builder.remote_output(ADDRESS_OF_ONE_CHUNK)


@stream
def b_naming_an_address_of_five_chunks(stream_builder: StreamBuilder) -> None:
    stream_builder.remote_output(ADDRESS_OF_FIVE_CHUNKS)


@stream
def b_linking_into_another_streams_input(stream_builder: StreamBuilder) -> None:
    """A link whose destination is named by address: links are only pulled."""
    pattern = _add_a_pattern(stream_builder)
    stream_builder.connect(
        pattern.output(PATTERN_PORT),
        stream_builder.remote_output(f"{SOURCE_STREAM_NAME}/{WINDOW_NODE}/{WINDOW_PORT}"),  # type: ignore[arg-type]
    )
