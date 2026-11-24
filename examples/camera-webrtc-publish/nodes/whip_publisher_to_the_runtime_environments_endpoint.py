# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A WHIP publisher whose endpoint and token come from the runtime's environment.

The WHIP URL is a credential, so it is never written into the graph document,
which the project's history keeps. Each node's process inherits `tatolabd`'s
environment, so the variables are set where the runtime starts.
"""

import os
from typing import Annotated

from tatolab.stream import (
    Description,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    InputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    node,
)
from tatolab.webrtc import WhipSessionPublishing

WHIP_URL_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_URL"
WHIP_BEARER_TOKEN_ENVIRONMENT_VARIABLE = "STREAMLIB_WHIP_BEARER_TOKEN"


def whip_session_publishing_to_the_runtime_environments_endpoint() -> WhipSessionPublishing:
    """A session publishing to `url`, with its `bearer_token` when one is set, from `tatolabd`'s environment."""
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
    return WhipSessionPublishing(whip_url, bearer_token)


@node.meta(
    description="Publishes encoded video and audio to the WHIP endpoint the runtime's environment names",
)
@node(paced_by_real_time=True)
class WhipPublisherToTheRuntimeEnvironmentsEndpoint:
    """A `WhipPublisher` session at `STREAMLIB_WHIP_URL`, sent `STREAMLIB_WHIP_BEARER_TOKEN`, with no setting."""

    tracks: InputPin[
        Annotated[
            Annotated[EncodedVideo, EncodedCodecs("h264")] | Annotated[EncodedAudio, EncodedCodecs("opus")],
            StreamDelivery("ordered"),
            Description("H.264 access units or Opus packets; each inbound link becomes one track"),
        ]
    ]

    def __init__(self) -> None:
        self._publishing = whip_session_publishing_to_the_runtime_environments_endpoint()

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._publishing.set_up(self.tracks)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        self._publishing.publish_the_next_message(self.tracks)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._publishing.close()
