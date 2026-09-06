# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two halves of a data track, meeting.

The publisher was proven against `publish_data_object` and the subscriber
against `ReceivedDataObject`, each against the wire contract alone. This is
where they meet: a producer's bag crosses a live link into the publisher's
`process()`, is classified and encoded, comes back as the object a
subscription would have delivered, is decoded, and crosses a second live link
to a consumer. What arrives is compared with what was sent.

What is shipped code here is the envelope: the publisher's classification and
encode, the subscriber's decode and write, and two real iceoryx2 links — the
bag has to survive the wire before the envelope has anything to carry.

Three layers are stood in for, and none of them is reachable from a test:

- **This wheel's own Rust sessions.** `MoqBroadcastPublishingSession` writes
  the object bytes and `MoqBroadcastSubscribingSession.next_media` hands them
  back, and both need a relay. So the object goes straight from the one to a
  hand-built `ReceivedDataObject` — a real one, minted by the same compiled
  module — and `_drain_until_stopped`'s dispatch on its type is not exercised
  here either.
- **`ctx.inputs`' link-name resolution.** `read_from_inbound_link_with_timestamp`
  lives on `LinkInputDataReader`, which app code cannot construct, and
  `ProcessorLinkDataAccess` exposes no inbound-link-name reader. The bag really
  crosses the link; the name it is filed under is supplied rather than read, so
  the publisher's per-link maps are keyed on a stated name.

The live fixture is what proves the relay hop, and it needs a rig and an
account.

Both link ends live on this thread because iceoryx2's ports are `!Send`, and
each is wired destination-first because a send with no subscriber attached is
dropped.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from streamlib import ProcessorLinkDataAccess, decode_msgpack_bytes_to_python_object
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber, _native
from streamlib_moq.processors import DATA_BAGS_OUTPUT_PORT, TRACKS_INPUT_PORT

A_RELAY = "https://relay.invalid/a-token"
A_BROADCAST = "streamlib/a-broadcast"
THE_DATA_TRACK_NAME = "telemetry"

#: What the producer publishes on, as a graph names it: the producing
#: processor's id, lowercased, then its port. It is the publisher's track key
#: and the name a refusal would carry, so it is spelled rather than invented.
THE_PRODUCERS_LINK = "kx7q2v1m8nz4/telemetry"

#: One bag exercising every shape the claim is about at once: `bytes` at the
#: top level and again inside a nested map, beside the scalar types a msgpack
#: round trip is free to widen.
A_TELEMETRY_BAG: "dict[str, Any]" = {
    "frame": 41,
    "note": "hello from the far side",
    "blob": b"\x00\xff\x10\x7f",
    "nested": {"deeper": {"payload": b"\xde\xad\xbe\xef"}, "series": [1, 2.5, None, True]},
}


class _ThePublishingSessionKeepingWhatItWasHanded:
    """`MoqBroadcastPublishingSession`, which needs a relay, standing still.

    It keeps each published object so the round trip can hand it to the
    subscriber as the `ReceivedDataObject` a subscription would have delivered,
    and answers the rest of the session surface `process()` and `teardown()`
    reach for.
    """

    def __init__(self) -> None:
        self.objects_published: "list[tuple[str, bytes]]" = []

    def publish_data_object(self, inbound_link_name: str, object_bytes: bytes) -> None:
        self.objects_published.append((inbound_link_name, object_bytes))

    def objects_the_delivery_deadline_shed(self) -> "list[tuple[str, int, int]]":
        return []

    def close(self) -> "str | None":
        return None


class _PublisherInputsOverALiveLink:
    """`ctx.inputs` reading the producer's own link rather than a stand-in."""

    def __init__(self, links: ProcessorLinkDataAccess, inbound_link: str) -> None:
        self._links = links
        self._inbound_link = inbound_link

    def inbound_link_names(self, port_name: str) -> "list[str]":
        assert port_name == TRACKS_INPUT_PORT
        return [self._inbound_link]

    def read_from_inbound_link_with_timestamp(
        self, port_name: str
    ) -> "tuple[Any, str, int] | None":
        bag, timestamp_ns = self._links.read_from_input_port_with_timestamp(port_name)
        if bag is None or timestamp_ns is None:
            return None
        return bag, self._inbound_link, timestamp_ns


class _PublisherContextOverALiveLink:
    runtime_id = "a-runtime"

    def __init__(self, inputs: _PublisherInputsOverALiveLink) -> None:
        self.inputs = inputs


class _SubscriberOutputsOverALiveLink:
    """`ctx.outputs` writing the subscriber's `data_bags` onto a real link."""

    def __init__(self, links: ProcessorLinkDataAccess) -> None:
        self._links = links

    def write(
        self, port_name: str, bag: "dict[str, Any]", timestamp_ns: "int | None" = None
    ) -> None:
        self._links.write_to_output_port(port_name, bag, timestamp_ns)


class DataTrackRoundTripUnderTest:
    """One bag's whole path, from the producer's write to the consumer's read.

    `send` puts a bag on the producer's link and drives the publisher; the
    standing-still publishing session hands each object straight to the
    subscriber, which decodes it and writes on `data_bags`. `receive` reads the
    far end.
    """

    def __init__(
        self,
        publisher: MoqBroadcastPublisher,
        publishing_session: _ThePublishingSessionKeepingWhatItWasHanded,
        subscriber: MoqBroadcastSubscriber,
        producer_links: ProcessorLinkDataAccess,
        publisher_links: ProcessorLinkDataAccess,
        subscriber_links: ProcessorLinkDataAccess,
        consumer_links: ProcessorLinkDataAccess,
    ) -> None:
        self._publisher = publisher
        self._publishing_session = publishing_session
        self._subscriber = subscriber
        self._producer_links = producer_links
        self._publisher_context = _PublisherContextOverALiveLink(
            _PublisherInputsOverALiveLink(publisher_links, THE_PRODUCERS_LINK)
        )
        self._subscriber_outputs = _SubscriberOutputsOverALiveLink(subscriber_links)
        self._consumer_links = consumer_links
        self._objects_routed = 0

    def send(self, bag: "dict[str, Any]", timestamp_ns: int) -> None:
        self._producer_links.write_to_output_port(THE_PRODUCERS_LINK, bag, timestamp_ns)
        self._publisher.process(self._publisher_context)  # type: ignore[arg-type]
        self._deliver_what_the_publishing_session_was_handed()

    def receive(self) -> "tuple[Any, int] | tuple[None, None]":
        return self._consumer_links.read_from_input_port_with_timestamp(
            DATA_BAGS_OUTPUT_PORT
        )

    @property
    def objects_published(self) -> "list[tuple[str, bytes]]":
        return self._publishing_session.objects_published

    def _deliver_what_the_publishing_session_was_handed(self) -> None:
        published = self._publishing_session.objects_published
        for track_name, object_bytes in published[self._objects_routed :]:
            self._subscriber._write_a_data_object(  # type: ignore[arg-type]
                _native.ReceivedDataObject(track_name, object_bytes),
                self._subscriber_outputs,
            )
        self._objects_routed = len(published)


@pytest.fixture
def data_track_round_trip(
    request: pytest.FixtureRequest,
) -> Iterator[DataTrackRoundTripUnderTest]:
    """The publisher and the subscriber with a live link at either end.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process — a fixed
    name would let one bad run poison every later one.
    """
    unique = f"moqdata{os.getpid()}_{request.node.name}"
    producer_links, publisher_links = _one_live_link(
        f"{unique}/tracks", THE_PRODUCERS_LINK, TRACKS_INPUT_PORT
    )
    subscriber_links, consumer_links = _one_live_link(
        f"{unique}/bags", DATA_BAGS_OUTPUT_PORT, DATA_BAGS_OUTPUT_PORT
    )

    publishing_session = _ThePublishingSessionKeepingWhatItWasHanded()
    publisher = MoqBroadcastPublisher(relay_url=A_RELAY, container_format="streamlib_bag")
    publisher._session = publishing_session  # type: ignore[assignment]
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY,
        broadcast=A_BROADCAST,
        container_format="streamlib_bag",
        data_track=THE_DATA_TRACK_NAME,
    )

    yield DataTrackRoundTripUnderTest(
        publisher,
        publishing_session,
        subscriber,
        producer_links,
        publisher_links,
        subscriber_links,
        consumer_links,
    )


def _one_live_link(
    channel_service_name: str, writing_port: str, reading_port: str
) -> "tuple[ProcessorLinkDataAccess, ProcessorLinkDataAccess]":
    """A writing end and a reading end joined by one iceoryx2 channel."""
    notify_service_name = f"{channel_service_name}/notify"
    link_id = f"L-{channel_service_name}"

    reading_end = ProcessorLinkDataAccess()
    reading_end.wire_input_link(
        reading_port,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        2,
        1,
        link_id,
    )
    writing_end = ProcessorLinkDataAccess()
    writing_end.wire_output_link(
        writing_port,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        link_id,
    )
    return writing_end, reading_end


def test_the_bag_a_producer_wrote_is_the_bag_the_far_end_reads(
    data_track_round_trip: DataTrackRoundTripUnderTest,
):
    """The whole claim in one assertion: nothing between the two ends changed
    the bag."""
    data_track_round_trip.send(A_TELEMETRY_BAG, timestamp_ns=7_000_000_000)

    received_bag, _ = data_track_round_trip.receive()

    assert received_bag == A_TELEMETRY_BAG


def test_bytes_survive_the_round_trip_as_bytes_at_every_depth(
    data_track_round_trip: DataTrackRoundTripUnderTest,
):
    """`==` alone would pass on a `bytes` that came back a `str` of the same
    characters, so the types are asserted where the bag nests them."""
    data_track_round_trip.send(A_TELEMETRY_BAG, timestamp_ns=7_000_000_000)

    received_bag, _ = data_track_round_trip.receive()

    assert received_bag is not None
    assert isinstance(received_bag["blob"], bytes)
    assert isinstance(received_bag["nested"]["deeper"]["payload"], bytes)
    assert received_bag["nested"]["deeper"]["payload"] == b"\xde\xad\xbe\xef"


def test_the_stamp_the_producer_wrote_is_the_stamp_the_far_end_reads(
    data_track_round_trip: DataTrackRoundTripUnderTest,
):
    """The producer's instant, carried by the envelope and restated as the
    write stamp — not the moment the subscriber happened to write."""
    data_track_round_trip.send(A_TELEMETRY_BAG, timestamp_ns=7_000_000_000)

    _, received_timestamp_ns = data_track_round_trip.receive()

    assert received_timestamp_ns == 7_000_000_000


def test_a_run_of_bags_arrives_whole_with_each_stamp_kept_and_its_index_minted(
    data_track_round_trip: DataTrackRoundTripUnderTest,
):
    """A single bag cannot show the per-link sequence minting, nor that one
    bag's keys do not leak into the next.

    The index is read off the envelope rather than the arriving bag, because it
    deliberately never enters one — it is the subscriber's material for
    counting gaps and nothing the consumer sees.
    """
    sent = [
        {"frame": frame, "blob": bytes([frame]) * 4, "nested": {"n": frame}}
        for frame in range(5)
    ]
    for frame, bag in enumerate(sent):
        data_track_round_trip.send(bag, timestamp_ns=1_000 + frame)

    received = [data_track_round_trip.receive() for _ in sent]

    assert received == [(bag, 1_000 + frame) for frame, bag in enumerate(sent)]
    published = data_track_round_trip.objects_published
    assert [track for track, _ in published] == [THE_PRODUCERS_LINK] * len(sent)
    assert [
        decode_msgpack_bytes_to_python_object(envelope)["sequence_index"]
        for _, envelope in published
    ] == list(range(len(sent)))
