# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The window contract reaching a helper-placed Python consumer's own wiring.

The contract crosses the parent→child wiring envelope beside `read_mode`, and
the child's own `InputMailboxesInner` — the same Rust the parent's mailboxes
run — resamples, mixes down and frames before `process()` ever sees a bag.
These tests read the envelope the way a child does; they need `/dev/shm` and
nothing else, so they run in CI.
"""

import pytest

from tatolab.runtime import _engine
from tatolab.stream import NodeLinkDataAccess

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def a_helper_process_data_plane() -> NodeLinkDataAccess:
    """The object a child builds for itself, with its own iceoryx2 node."""
    return _engine.open_node_link_data_access_for_helper_process()


def wire_with_window(data_plane: NodeLinkDataAccess, audio_window) -> None:
    data_plane.wire_input_link(
        "audio",
        "streamlib/tests/audio-window/never-opened",
        "streamlib/tests/audio-window/never-opened",
        "streamlib/tests/audio-window/never-notified",
        "read_next_in_order",
        16,
        16,
        2,
        1,
        "L-test",
        audio_window,
    )


def test_a_child_refuses_a_window_contract_whose_field_it_cannot_read():
    """The parent sends the contract resolved; a key it got wrong is named here
    rather than surfacing as an anonymous decode failure."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(data_plane, {"sample_rate": 16_000, "channels": 1})

    rendered = str(refusal.value)
    assert "audio" in rendered and "dtype" in rendered, (
        f"the refusal must name the port and the field; got {rendered}"
    )


def test_a_child_reads_a_contract_that_follows_the_sources_channels():
    """The count is the one value the envelope may spell as a word, and the
    child must read it rather than refusing what the parent legitimately sent."""
    data_plane = a_helper_process_data_plane()

    wire_with_window(
        data_plane,
        {
            "sample_rate": 48_000,
            "channels": "source",
            "dtype": "f32",
            "window_size": 960,
            "hop": 960,
        },
    )


def test_a_child_reads_a_contract_whose_channels_key_the_parent_omitted():
    """An omitted key means the same thing as the spelled one, so a terser
    writer is not refused for terseness."""
    data_plane = a_helper_process_data_plane()

    wire_with_window(
        data_plane,
        {"sample_rate": 48_000, "dtype": "f32", "window_size": 960, "hop": 960},
    )


def test_a_child_refuses_a_channels_value_that_names_no_count():
    """A word that is not the one spelling is a writer that meant something
    else, and guessing which count is the reshaping the contract refuses."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 48_000,
                "channels": "stereo",
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            },
        )

    rendered = str(refusal.value)
    assert "channels" in rendered and "source" in rendered, (
        f"the refusal must name the field and the spelling that works; got {rendered}"
    )


def test_a_child_refuses_a_bool_where_a_channel_count_belongs():
    """`bool` is an `int` subclass, so `True` would otherwise wire as one
    channel — a plausible count nobody wrote. The declaration constructor
    refuses one by name; the envelope must too, since nothing else guards it."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(TypeError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 48_000,
                "channels": True,
                "dtype": "f32",
                "window_size": 960,
                "hop": 960,
            },
        )

    rendered = str(refusal.value)
    assert "channels" in rendered and "bool" in rendered, (
        f"the refusal must name the field and the kind; got {rendered}"
    )


def test_a_child_refuses_a_window_contract_the_stage_could_not_honour():
    """The same validator both languages' declaration paths call, applied again
    where the child receives the contract: a hop above the window would silently
    discard the samples between windows."""
    data_plane = a_helper_process_data_plane()

    with pytest.raises(ValueError) as refusal:
        wire_with_window(
            data_plane,
            {
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 1_024,
            },
        )

    rendered = str(refusal.value)
    assert "512" in rendered and "1024" in rendered, (
        f"the refusal must name both numbers; got {rendered}"
    )
