# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `WhipPublisher` whose endpoint and token come from the runtime's environment.

The WHIP URL is a credential, so it is never written into the graph document,
which the project's history keeps. Each node's process inherits `tatolabd`'s
environment, so the variables are set where the runtime starts.
"""

import os

from tatolab.stream import node
from tatolab.webrtc import WhipPublisher, WhipPublisherConfig

WHIP_URL_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_URL"
WHIP_BEARER_TOKEN_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_BEARER_TOKEN"


def whip_publisher_config_from_the_runtime_environment() -> WhipPublisherConfig:
    """The publisher's `url`, and its `bearer_token` when one is set, from `tatolabd`'s environment."""
    whip_url = os.environ.get(WHIP_URL_ENVIRONMENT_VARIABLE)
    if not whip_url:
        raise ValueError(
            f"{WHIP_URL_ENVIRONMENT_VARIABLE} is unset in the runtime's environment. "
            f"It is the WHIP endpoint to publish to — for Cloudflare Stream, the "
            f"`.../webRTC/publish` URL its dashboard shows for a live input — and a "
            f"credential: set it where tatolabd starts, never in the graph document."
        )
    # Cloudflare Stream authenticates by the key in the URL's path and needs no
    # token; an endpoint that wants RFC 9725's `Authorization: Bearer` takes one.
    bearer_token = os.environ.get(WHIP_BEARER_TOKEN_ENVIRONMENT_VARIABLE) or None
    return WhipPublisherConfig(url=whip_url, bearer_token=bearer_token)


@node.meta(
    description="Publishes encoded video and audio to the WHIP endpoint the runtime's environment names",
)
@node.process(paced_by_real_time=True)
class WhipPublisherToTheRuntimeEnvironmentsEndpoint(WhipPublisher):
    """`WhipPublisher`, configured from `STREAMLIB_WHIP_URL` and `STREAMLIB_WHIP_BEARER_TOKEN`."""

    def __init__(self) -> None:
        super().__init__(whip_publisher_config_from_the_runtime_environment())
