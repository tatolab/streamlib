# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors as a graph sees them.

`rt.add` with no adapter and no engine change is the whole claim of the
extension model, so it is what these check — over a real `Runtime`, which needs
no device to build a graph. Nothing here reaches a relay.
"""

from unittest import mock

import pytest

import streamlib
from streamlib import log
from streamlib import H264Decoder, H264Encoder, OpusDecoder, OpusEncoder
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber
from streamlib_moq.processors import (
    CONTAINER_FORMATS,
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    READER_THREAD_JOIN_TIMEOUT_SECONDS,
    SUBSCRIBER_POLL_TIMEOUT_MS,
    describe_the_delivery_deadline,
    describe_what_the_delivery_deadline_shed,
    track_medium_of_codec,
)

A_RELAY = "https://relay.invalid/a-token"
A_BROADCAST = "streamlib/a-broadcast"

PUBLISHER_CONFIG = {"relay_url": A_RELAY}
SUBSCRIBER_CONFIG = {
    "relay_url": A_RELAY,
    "broadcast": A_BROADCAST,
    "video_track": "1.m4s",
    "audio_track": "2.m4s",
}


@pytest.fixture
def runtime():
    runtime = streamlib.Runtime()
    try:
        yield runtime
    finally:
        runtime.shutdown()


@pytest.mark.parametrize(
    ("processor_class", "config"),
    [(MoqBroadcastPublisher, PUBLISHER_CONFIG), (MoqBroadcastSubscriber, SUBSCRIBER_CONFIG)],
)
def test_an_installed_extensions_processor_is_added_like_any_other(
    runtime, processor_class, config
):
    added = runtime.add(processor_class, config=config)

    assert added.display_name == processor_class.__name__


def test_the_publisher_wires_to_both_encoders_without_an_adapter(runtime):
    """One fan-in port takes both encoders, which is what makes a broadcast
    with video and audio a matter of wiring rather than of config."""
    video_encoder = runtime.add(H264Encoder)
    audio_encoder = runtime.add(OpusEncoder)
    publisher = runtime.add(MoqBroadcastPublisher, config=PUBLISHER_CONFIG)

    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


def test_the_subscriber_wires_to_both_decoders_without_an_adapter(runtime):
    subscriber = runtime.add(MoqBroadcastSubscriber, config=SUBSCRIBER_CONFIG)
    video_decoder = runtime.add(H264Decoder)
    audio_decoder = runtime.add(OpusDecoder)

    runtime.connect(
        subscriber.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(
        subscriber.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


@pytest.mark.parametrize("container_format", CONTAINER_FORMATS)
def test_both_container_formats_are_addable(runtime, container_format):
    added = runtime.add(
        MoqBroadcastPublisher,
        config={**PUBLISHER_CONFIG, "container_format": container_format},
    )

    assert added.display_name == "MoqBroadcastPublisher"


def test_a_container_format_this_wheel_does_not_write_is_refused_by_name():
    with pytest.raises(ValueError, match="container_format"):
        MoqBroadcastPublisher(relay_url=A_RELAY, container_format="mpegts")  # type: ignore[arg-type]


def test_a_publisher_without_a_relay_is_refused_by_name():
    with pytest.raises(ValueError, match="relay_url"):
        MoqBroadcastPublisher(relay_url="")


def test_the_relay_refusal_says_where_a_draft_16_token_goes():
    """Draft-16 provisions relays per account and carries the token in the URL
    path, so a bare host is not a usable endpoint and the message has to say
    so — there is nowhere else to learn it."""
    with pytest.raises(ValueError, match="token"):
        MoqBroadcastSubscriber(relay_url="", broadcast=A_BROADCAST, video_track="1.m4s")


def test_a_subscriber_naming_no_track_at_all_is_refused_by_name():
    """Two static output ports and no track named for either would subscribe to
    nothing and produce nothing, which reads from outside as a hang."""
    with pytest.raises(ValueError, match="video_track"):
        MoqBroadcastSubscriber(relay_url=A_RELAY, broadcast=A_BROADCAST)


def test_a_subscriber_may_name_one_track_and_leave_the_other_port_silent():
    MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="1.m4s"
    )


@pytest.mark.parametrize(
    ("codec", "medium"),
    [("h264", "video"), ("h265", "video"), ("opus", "audio")],
)
def test_a_bags_codec_settles_which_medium_its_link_carries(codec, medium):
    assert track_medium_of_codec(codec, "encoder/out") == medium


def test_a_codec_this_broadcast_does_not_carry_is_refused_naming_the_link():
    """A graph that wired something other than an encoder into the publisher is
    a wiring mistake, and this message is the only place it is catchable."""
    with pytest.raises(ValueError, match="encoder/out"):
        track_medium_of_codec("vp9", "encoder/out")


def test_a_missing_codec_is_refused_rather_than_guessed_at():
    with pytest.raises(ValueError, match="codec"):
        track_medium_of_codec(None, "encoder/out")


#: A helper's teardown reply and its exit are each bounded at five seconds by
#: the engine, and `stop()` runs inside the first of those.
HELPER_TEARDOWN_BUDGET_SECONDS = 5.0


def test_a_stop_completes_inside_the_helpers_teardown_budget():
    """The two waits a `stop()` can sit through are the reading thread's own
    poll and the join that follows it; together they must leave the budget
    room, or the helper is killed rather than stopped."""
    longest_stop_seconds = (
        SUBSCRIBER_POLL_TIMEOUT_MS / 1000 + READER_THREAD_JOIN_TIMEOUT_SECONDS
    )

    assert longest_stop_seconds < HELPER_TEARDOWN_BUDGET_SECONDS


def test_a_bag_past_the_link_ceiling_is_reported_once_and_not_every_frame():
    """The engine drops an oversized bag at `debug` rather than raising, so a
    subscriber that said nothing would look like a stream that just stopped —
    but saying it per frame would bury the log of a stream that never
    recovers."""
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="1.m4s"
    )
    said: "list[str]" = []
    with mock.patch.object(log, "error", said.append):
        for _ in range(3):
            subscriber._report_a_bag_the_link_will_drop(
                "encoded_video", b"x" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)
            )

    assert len(said) == 1
    assert "Reported once" in said[0]


def test_a_publisher_carrying_a_delivery_deadline_is_added_like_any_other(runtime):
    added = runtime.add(
        MoqBroadcastPublisher,
        config={**PUBLISHER_CONFIG, "delivery_deadline_ms": 250},
    )

    assert added.display_name == "MoqBroadcastPublisher"


@pytest.mark.parametrize("not_a_deadline", ["250", 2.5, True, -1])
def test_a_delivery_deadline_that_is_not_a_count_of_milliseconds_is_refused_by_name(
    not_a_deadline,
):
    """`bool` is an `int` in Python, so `True` would otherwise read as a
    one-millisecond deadline that sheds every frame but the sync points."""
    with pytest.raises(ValueError, match="delivery_deadline_ms"):
        MoqBroadcastPublisher(relay_url=A_RELAY, delivery_deadline_ms=not_a_deadline)


def test_a_run_that_shed_nothing_says_so_rather_than_saying_nothing():
    """A silently shed frame is the failure mode this wheel is careful about,
    so the absence of drops is reported as loudly as their presence."""
    assert (
        describe_what_the_delivery_deadline_shed([])
        == "the delivery deadline shed nothing"
    )


def test_the_shed_report_names_each_link_with_its_objects_and_its_bytes():
    said = describe_what_the_delivery_deadline_shed(
        [("camera", 12, 48213), ("second_camera", 1, 900)]
    )

    assert said == (
        "the delivery deadline shed camera=12 objects/48213 bytes, "
        "second_camera=1 objects/900 bytes"
    )


@pytest.mark.parametrize(
    ("delivery_deadline_ms", "said"),
    [
        (None, "no delivery deadline is configured"),
        (0, "the delivery deadline is 0 ms"),
        (250, "the delivery deadline is 250 ms"),
    ],
)
def test_the_deadline_a_publisher_runs_under_is_said_where_it_is_configured(
    delivery_deadline_ms, said
):
    """A run's log has to state which arm it is, or a measured before/after
    cannot be told apart after the fact."""
    assert describe_the_delivery_deadline(delivery_deadline_ms) == said


class _SessionThatAnswers:
    """A publishing session whose every publish call gives one fixed answer."""

    def __init__(self, reaches_the_transport: bool) -> None:
        self._answer = reaches_the_transport
        self.calls = 0

    def publish_video_access_unit(self, *args, **kwargs) -> bool:
        del args, kwargs
        self.calls += 1
        return self._answer

    def objects_the_delivery_deadline_shed(self) -> "list[tuple[str, int, int]]":
        return [] if self._answer else [("camera", self.calls, 900 * self.calls)]

    def close(self) -> "str | None":
        return None


class _InputsReadingOneVideoBag:
    def read_from_inbound_link_with_timestamp(self, port: str):
        assert port == "tracks"
        return (
            {
                "codec": "h264",
                "bitstream": b"\x00\x00\x00\x01\x65",
                "is_sync_point": True,
                "group_index": 0,
                "sequence_index": 0,
                "width": 320,
                "height": 180,
            },
            "camera",
            5_000_000_000,
        )


class _ContextReadingOneVideoBag:
    inputs = _InputsReadingOneVideoBag()


def _drive_one_bag_through(reaches_the_transport: bool) -> "tuple[list[str], int]":
    publisher = MoqBroadcastPublisher(relay_url=A_RELAY)
    session = _SessionThatAnswers(reaches_the_transport)
    publisher._session = session  # type: ignore[assignment]
    said: "list[str]" = []
    with mock.patch.object(log, "info", said.append):
        publisher.process(_ContextReadingOneVideoBag())  # type: ignore[arg-type]
        publisher.teardown(None)  # type: ignore[arg-type]
    return said, session.calls


def test_a_bag_the_deadline_shed_is_neither_counted_as_published_nor_announced_as_the_first():
    """The drop must be counted and said — and the numbers beside it must not
    lie: a shed bag reached nothing, so it is not a publish."""
    said, calls = _drive_one_bag_through(reaches_the_transport=False)

    assert calls == 1
    assert not any("first bag accepted" in line for line in said)
    assert any(
        "bags_published=0" in line and "shed camera=1 objects/900 bytes" in line
        for line in said
    ), said


def test_a_bag_that_reached_the_transport_is_counted_and_announced_as_the_first():
    said, calls = _drive_one_bag_through(reaches_the_transport=True)

    assert calls == 1
    assert any("first bag accepted by the broadcast" in line for line in said)
    assert any(
        "bags_published=1" in line and "shed nothing" in line for line in said
    ), said
