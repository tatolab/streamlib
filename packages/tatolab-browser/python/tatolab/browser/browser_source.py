# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`BrowserSource`: a web page rendered offscreen by Chromium, as a stream source.

`start()` lends the node's ports to this wheel's Rust, which opens the page as
a tab in the machine's browser host: one Chromium (CEF) the runtime runs for
every stream, giving each stream a profile of its own. Each frame the host
paints is written from host memory into a texture from the node's output pool
and published as an Image frame; the page's audio is published on `audio` as
planar f32 Channels blocks;
every `data` message reaches the page as a `tatolab:data` DOM event, and every
`window.tatolab.publish(object)` call is written to `events`. Each wired
`video_in_N` and `audio_in_N` reaches the page as a `MediaStreamTrack`,
`window.tatolab.inputTrack("video_in_0")`, which a page draws or plays like any
camera or microphone track; an unwired one is `null`. When the host
ends, the runtime starts it again and the page opens again in it, receiving the
latest `data` messages that arrived meanwhile, up to 256 of them and 64 MiB.
A page the host can no longer render, or a host the runtime gives up on, fails
the node. Python runs at start and stop, never per frame.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path
from typing import Annotated
from urllib.parse import urlsplit

from tatolab.stream import (
    Channels,
    Image,
    Input,
    Output,
    Records,
    RuntimeContextFullAccess,
    from_config,
    log,
    node,
)

from . import _native

VIDEO_OUTPUT_PORT = "video"
EVENTS_OUTPUT_PORT = "events"
AUDIO_OUTPUT_PORT = "audio"
DATA_INPUT_PORT = "data"
VIDEO_INPUT_PORTS = ("video_in_0", "video_in_1", "video_in_2", "video_in_3")
AUDIO_INPUT_PORTS = ("audio_in_0", "audio_in_1", "audio_in_2", "audio_in_3")
PAINTED_PAGE_TEXTURE_FORMAT = "rgba8_unorm"


@dataclasses.dataclass
class BrowserSourceConfig:
    """What a `BrowserSource` is configured with."""

    url: Annotated[
        str, "The page to render: an http(s), file or data URL, or a file path inside the project."
    ] = "about:blank"
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


def page_url_of(url_or_project_path: str) -> str:
    """The URL the browser host opens: `url_or_project_path` itself when it has a scheme, else
    the file it names, relative to the project directory every node's process runs in."""
    if urlsplit(url_or_project_path).scheme:
        return url_or_project_path
    page_path = Path(url_or_project_path).resolve()
    if not page_path.is_file():
        raise FileNotFoundError(
            f"BrowserSource: `url` {url_or_project_path!r} has no scheme, so it names a file "
            f"in the project, and {page_path} is not one"
        )
    return page_path.as_uri()


@node.meta(
    description="Renders a web page offscreen with Chromium and publishes its frames, audio and events",
)
@node.process(execution="manual")
class BrowserSource:
    """A web page, published as RGBA Image frames, Channels blocks and Records."""

    def __init__(self, config: BrowserSourceConfig) -> None:
        self._config = dataclasses.replace(config, url=page_url_of(config.url))
        self._rendering: "_native.BrowserPageRendering | None" = None

    data = Input(
        Records,
        delivery="ordered",
        description="One map per message, dispatched to the page as `tatolab:data`",
    )
    video_in_0 = Input(
        Image,
        delivery="newest",
        description='A video the page takes as `window.tatolab.inputTrack("video_in_0")`',
    )
    video_in_1 = Input(
        Image,
        delivery="newest",
        description='A video the page takes as `window.tatolab.inputTrack("video_in_1")`',
    )
    video_in_2 = Input(
        Image,
        delivery="newest",
        description='A video the page takes as `window.tatolab.inputTrack("video_in_2")`',
    )
    video_in_3 = Input(
        Image,
        delivery="newest",
        description='A video the page takes as `window.tatolab.inputTrack("video_in_3")`',
    )
    audio_in_0 = Input(
        Channels,
        delivery="ordered",
        description='Audio the page takes as `window.tatolab.inputTrack("audio_in_0")`',
    )
    audio_in_1 = Input(
        Channels,
        delivery="ordered",
        description='Audio the page takes as `window.tatolab.inputTrack("audio_in_1")`',
    )
    audio_in_2 = Input(
        Channels,
        delivery="ordered",
        description='Audio the page takes as `window.tatolab.inputTrack("audio_in_2")`',
    )
    audio_in_3 = Input(
        Channels,
        delivery="ordered",
        description='Audio the page takes as `window.tatolab.inputTrack("audio_in_3")`',
    )
    video = Output(
        Image,
        formats=(PAINTED_PAGE_TEXTURE_FORMAT,),
        description="The painted page, in sRGB",
    )
    audio = Output(
        Channels,
        rate=from_config("audio_sample_rate"),
        dtype="f32",
        description="The page's audio; a silent page publishes none",
    )
    events = Output(
        Records,
        description="One map per `window.tatolab.publish(object)` call",
    )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        wired_video_input_ports = [
            port for port in VIDEO_INPUT_PORTS if ctx.inputs.inbound_link_names(port)
        ]
        wired_audio_input_ports = [
            port for port in AUDIO_INPUT_PORTS if ctx.inputs.inbound_link_names(port)
        ]
        module_host_table = ctx.open_module_host_table(
            output_ports=[VIDEO_OUTPUT_PORT, EVENTS_OUTPUT_PORT, AUDIO_OUTPUT_PORT],
            input_ports=[DATA_INPUT_PORT, *wired_video_input_ports, *wired_audio_input_ports],
        )
        config = self._config
        self._rendering = _native.BrowserPageRendering(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            EVENTS_OUTPUT_PORT,
            AUDIO_OUTPUT_PORT,
            DATA_INPUT_PORT,
            wired_video_input_ports,
            wired_audio_input_ports,
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
            f"up to {config.frame_rate} fps, audio at {config.audio_sample_rate} Hz, "
            f"inputs {wired_video_input_ports + wired_audio_input_ports}"
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
            f"frames_published_from_painted_textures="
            f"{rendering.frames_published_from_painted_textures} "
            f"frames_dropped={rendering.frames_dropped} "
            f"audio_blocks_published={rendering.audio_blocks_published} "
            f"events_published={rendering.events_published} "
            f"data_messages_delivered={rendering.data_messages_delivered} "
            f"data_messages_refused={rendering.data_messages_refused} "
            f"page_input_video_frames_delivered={rendering.page_input_video_frames_delivered} "
            f"page_input_audio_blocks_delivered={rendering.page_input_audio_blocks_delivered} "
            f"page_input_chunks_refused={rendering.page_input_chunks_refused} "
            f"browser_host_reconnects={rendering.browser_host_reconnects}"
        )
        self._rendering = None
