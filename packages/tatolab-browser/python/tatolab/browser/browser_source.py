# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`BrowserSource`: a web page rendered offscreen by Chromium, as a stream source.

`start()` lends the node's ports to this wheel's Rust, which opens the page as
a tab in the machine's browser host: one Chromium (CEF) the runtime runs for
every stream, giving each stream a profile of its own. Each frame the host
paints is written from host memory into a texture from the node's output pool
and published as a `VideoFrame`; the page's audio is published on `audio`;
every `data` message reaches the page as a `tatolab:data` DOM event, and every
`window.tatolab.publish(object)` call is written to `events`. When the host
ends, the runtime starts it again and the page opens again in it. Python runs
at start and stop, never per frame.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import RuntimeContextFullAccess, VideoFrame, log, node
from tatolab.stream.catalog import BinaryEntry

from . import _native

VIDEO_OUTPUT_PORT = "video"
EVENTS_OUTPUT_PORT = "events"
AUDIO_OUTPUT_PORT = "audio"
DATA_INPUT_PORT = "data"


@dataclasses.dataclass
class BrowserSourceConfig:
    """What a `BrowserSource` is configured with."""

    url: Annotated[str, "The page to render: http(s), file or data URL."]
    width: Annotated[int, "The page's width in pixels, and each frame's."] = 1920
    height: Annotated[int, "The page's height in pixels, and each frame's."] = 1080
    frame_rate: Annotated[
        int, "The most frames a second CEF paints, 1 to 60; a still page paints none."
    ] = 60
    transparent_background: Annotated[
        bool,
        "Paint the page over transparency, so it composites as an overlay; "
        "otherwise over opaque white.",
    ] = True
    audio_sample_rate: Annotated[int, "The rate the page's audio is published at."] = 48_000
    output_pool_rotation_depth: Annotated[
        int, "How many painted frames the output pool rotates before reusing a free slot."
    ] = 4


@node(
    execution="manual",
    description="Renders a web page offscreen with Chromium and publishes its frames, audio and events",
)
class BrowserSource:
    """A web page, published as RGBA `VideoFrame`s, audio blocks and event maps."""

    def __init__(self, config: BrowserSourceConfig) -> None:
        self._config = config
        self._rendering: "_native.BrowserPageRendering | None" = None

    @node.input(delivery_profile="ordered")
    def data(self) -> bytes:
        """One msgpack map per message, dispatched to the page as `tatolab:data`."""
        ...

    @node.output(description="The painted page, as VideoFrame records")
    def video(self) -> VideoFrame: ...

    @node.output(catalog=BinaryEntry(mode="snapshot", mime="application/msgpack"))
    def audio(self) -> None:
        """Timestamped blocks of the page's interleaved f32 audio; a silent page publishes none."""

    @node.output(catalog=BinaryEntry(mode="snapshot", mime="application/msgpack"))
    def events(self) -> None:
        """One msgpack map per `window.tatolab.publish(object)` call."""

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(
            output_ports=[VIDEO_OUTPUT_PORT, EVENTS_OUTPUT_PORT, AUDIO_OUTPUT_PORT],
            input_ports=[DATA_INPUT_PORT],
        )
        config = self._config
        self._rendering = _native.BrowserPageRendering(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            EVENTS_OUTPUT_PORT,
            AUDIO_OUTPUT_PORT,
            DATA_INPUT_PORT,
            config.url,
            config.width,
            config.height,
            config.frame_rate,
            config.transparent_background,
            config.audio_sample_rate,
            config.output_pool_rotation_depth,
        )
        log.info(
            f"BrowserSource: rendering {config.url} at {config.width}x{config.height}, "
            f"up to {config.frame_rate} fps, audio at {config.audio_sample_rate} Hz"
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._close_the_page()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._close_the_page()

    def _close_the_page(self) -> None:
        rendering = self._rendering
        if rendering is None:
            return
        rendering.close()
        log.info(
            f"BrowserSource: page closed. frames_published={rendering.frames_published} "
            f"frames_dropped={rendering.frames_dropped} "
            f"audio_blocks_published={rendering.audio_blocks_published} "
            f"events_published={rendering.events_published} "
            f"data_messages_delivered={rendering.data_messages_delivered} "
            f"data_messages_refused={rendering.data_messages_refused} "
            f"browser_host_reconnects={rendering.browser_host_reconnects}"
        )
        self._rendering = None
