# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A stream that sends a port to a relay and reads it back there, through gateway nodes.

One gateway publishes the counting source's records as a track of its
broadcast; the track reaches its send through a graph variable. A second
gateway, its own connection to the same relay, reads that track with
`MoqTrackReceive` into a sink. Nothing joins the two halves but the relay.
`STREAMLIB_FIXTURE_RELAY_URL`, in the environment the stream compiles in, names it.
"""

import os

from moq_gateway_nodes import CountingRecordsSource, ReportsEachRecordReceivedSink
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackReceive, MoqTrackSend
from tatolab.stream import GetVariable, SetVariable, StreamBuilder, graph_generator

FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE = "STREAMLIB_FIXTURE_RELAY_URL"
SENDING_BROADCAST = "suite-gateway-sender"
RECEIVING_BROADCAST = "suite-gateway-receiver"
COUNT_TRACK = "counts"
#: The name a test renames `count_track`'s track to while the stream plays.
RENAMED_COUNT_TRACK = "recounts"


@graph_generator
def counting_records_sent_to_the_relay_and_read_back(stream_builder: StreamBuilder) -> None:
    relay_url = os.environ[FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE]
    source = stream_builder.add(CountingRecordsSource, name="counts")
    sending_gateway = stream_builder.add(
        MoqGateway,
        name="sending_gateway",
        input_values={"kind": "moq-relay", "relay_url": relay_url, "broadcast": SENDING_BROADCAST},
    )
    count_track = stream_builder.add(MoqTrack, name="count_track", input_values={"track": COUNT_TRACK})
    stream_builder.connect(sending_gateway.output("connection"), count_track.input("gateway"))
    set_the_track = stream_builder.add(SetVariable, input_values={"variable": "count_track"})
    stream_builder.connect(count_track.output("connection"), set_the_track.input("value"))
    get_the_track = stream_builder.add(GetVariable, input_values={"variable": "count_track"})
    send = stream_builder.add(MoqTrackSend, name="send")
    stream_builder.connect(get_the_track.output("value"), send.input("track"))
    receiving_gateway = stream_builder.add(
        MoqGateway,
        name="receiving_gateway",
        input_values={"kind": "moq-relay", "relay_url": relay_url, "broadcast": RECEIVING_BROADCAST},
    )
    receive = stream_builder.add(
        MoqTrackReceive,
        name="receive",
        input_values={"broadcast": f"{SENDING_BROADCAST}.hang", "track": COUNT_TRACK},
    )
    stream_builder.connect(receiving_gateway.output("connection"), receive.input("gateway"))
    sink = stream_builder.add(ReportsEachRecordReceivedSink, name="sink")

    stream_builder.connect(source.output("records_to_downstream"), send.input("content"))
    stream_builder.connect(receive.output("content"), sink.input("records_from_upstream"))


#: The receive node of `a_receive_reading_the_count_track_alone`, its `content` linked to nothing.
UNLINKED_RECEIVE = "receive_alone"


@graph_generator
def a_receive_reading_the_count_track_alone(stream_builder: StreamBuilder) -> None:
    relay_url = os.environ[FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE]
    receiving_gateway = stream_builder.add(
        MoqGateway,
        name="receiving_gateway",
        input_values={"kind": "moq-relay", "relay_url": relay_url, "broadcast": RECEIVING_BROADCAST},
    )
    receive = stream_builder.add(
        MoqTrackReceive,
        name=UNLINKED_RECEIVE,
        input_values={"broadcast": f"{SENDING_BROADCAST}.hang", "track": COUNT_TRACK},
    )
    stream_builder.connect(receiving_gateway.output("connection"), receive.input("gateway"))
