# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream that sends a port to a relay and reads it back there, through gateway nodes.

One gateway publishes the counting source's records as a track of its
broadcast; a second gateway, its own connection to the same relay, reads that
track with `MoqTrackReceive` into a sink. Nothing joins the two halves but the
relay. `STREAMLIB_FIXTURE_RELAY_URL`, in the environment the stream compiles in, names it.
"""

import os

from moq_gateway_processors import CountingRecordsSource, ReportsEachRecordReceivedSink
from tatolab.moq import MoqGateway, MoqTrack, MoqTrackReceive, MoqTrackSend
from tatolab.stream import StreamBuilder, graph_generator

FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE = "STREAMLIB_FIXTURE_RELAY_URL"
SENDING_BROADCAST = "suite-gateway-sender"
RECEIVING_BROADCAST = "suite-gateway-receiver"
COUNT_TRACK = "counts"


@graph_generator
def counting_records_sent_to_the_relay_and_read_back(stream_builder: StreamBuilder) -> None:
    relay_url = os.environ[FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE]
    source = stream_builder.add(CountingRecordsSource, name="counts")
    sending_gateway = stream_builder.add(
        MoqGateway,
        name="sending_gateway",
        config={"kind": "moq-relay", "relay_url": relay_url, "broadcast": SENDING_BROADCAST},
    )
    count_track = stream_builder.add(
        MoqTrack, name="count_track", config={"gateway": sending_gateway, "track": COUNT_TRACK}
    )
    send = stream_builder.add(MoqTrackSend, name="send", config={"track": count_track})
    receiving_gateway = stream_builder.add(
        MoqGateway,
        name="receiving_gateway",
        config={"kind": "moq-relay", "relay_url": relay_url, "broadcast": RECEIVING_BROADCAST},
    )
    receive = stream_builder.add(
        MoqTrackReceive,
        name="receive",
        config={
            "gateway": receiving_gateway,
            "broadcast": f"{SENDING_BROADCAST}.hang",
            "track": COUNT_TRACK,
            "family": "records",
        },
    )
    sink = stream_builder.add(ReportsEachRecordReceivedSink, name="sink")

    stream_builder.connect(source.output("records_to_downstream"), send.input("content"))
    stream_builder.connect(receive.output("content"), sink.input("records_from_upstream"))
