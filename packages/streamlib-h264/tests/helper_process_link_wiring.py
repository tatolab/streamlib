# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One link between two helper-process link endpoints, wired directly with no runtime."""

from tatolab.stream import NodeLinkDataAccess

CHANNEL_SERVICE_CREATION_DEPTH = 8
INPUT_PORT_RING_DEPTH = 8
MAX_SUBSCRIBERS = 2
EXPECTED_PAYLOAD_BYTES = 1024
MAX_PAYLOAD_BYTES_PER_CHANNEL = 1 << 20


def wire_one_link(
    source: NodeLinkDataAccess,
    output_port: str,
    destination: NodeLinkDataAccess,
    input_port: str,
    channel_service_name: str,
    destination_notify_service_name: str,
    notify_max_notifiers: int = 1,
) -> None:
    """Join `source`'s `output_port` to `destination`'s `input_port`, read in order.

    Every link into one destination shares its notify service, so
    `notify_max_notifiers` counts the links feeding it. The destination
    subscribes first: iceoryx2 drops a send with no subscriber attached.
    """
    link_id = f"L-{channel_service_name}"
    destination.wire_input_link(
        input_port,
        channel_service_name,
        channel_service_name,
        destination_notify_service_name,
        "read_next_in_order",
        CHANNEL_SERVICE_CREATION_DEPTH,
        INPUT_PORT_RING_DEPTH,
        MAX_SUBSCRIBERS,
        notify_max_notifiers,
        link_id,
    )
    source.wire_output_link(
        output_port,
        channel_service_name,
        destination_notify_service_name,
        EXPECTED_PAYLOAD_BYTES,
        MAX_PAYLOAD_BYTES_PER_CHANNEL,
        CHANNEL_SERVICE_CREATION_DEPTH,
        MAX_SUBSCRIBERS,
        notify_max_notifiers,
        link_id,
    )
