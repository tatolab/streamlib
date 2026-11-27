# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`BrowserSource`: a web page rendered offscreen by Chromium, as a stream source.

The node's body lends the node's ports to this wheel's Rust once. The page is a
computed value: each build opens it as a tab in the machine's browser host, one
Chromium (CEF) the runtime runs for every stream, giving each stream a profile
of its own. Each frame the host
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
the node. Python never runs per frame.

Changing any input while the stream plays opens a new tab off the frame thread,
while the page before keeps publishing; the new one is taken at the node's next
tick and the tab before is closed. A size change publishes into slots of the
new size, and the output pool releases the old ones once nothing holds them. A
page that fails to open leaves the one before publishing. Which `video_in_N` and
`audio_in_N` are wired is read as each tab opens.
"""

from __future__ import annotations

from pathlib import Path
from typing import Annotated
from urllib.parse import urlsplit

from tatolab.stream import (
    Audio,
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    computed,
    log,
    node,
)

from . import _native
from .page_message import PageMessage

VIDEO_OUTPUT_PORT = "video"
EVENTS_OUTPUT_PORT = "events"
AUDIO_OUTPUT_PORT = "audio"
DATA_INPUT_PORT = "data"
VIDEO_INPUT_PORTS = ("video_in_0", "video_in_1", "video_in_2", "video_in_3")
AUDIO_INPUT_PORTS = ("audio_in_0", "audio_in_1", "audio_in_2", "audio_in_3")
PAINTED_PAGE_TEXTURE_FORMAT = "rgba8_unorm"


class BrowserPageFileMissing(FileNotFoundError):
    """`url` has no scheme, so it names a file inside the project, and that file is not there."""


def page_url_of(url_or_project_path: str) -> str:
    """The URL the browser host opens: `url_or_project_path` itself when it has a scheme, else
    the file it names, relative to the project directory every node's process runs in."""
    if urlsplit(url_or_project_path).scheme:
        return url_or_project_path
    page_path = Path(url_or_project_path).resolve()
    if not page_path.is_file():
        raise BrowserPageFileMissing(
            f"BrowserSource: `url` {url_or_project_path!r} has no scheme, so it names a file "
            f"in the project, and {page_path} is not one"
        )
    return page_path.as_uri()


class PageOpenInTheBrowserHost:
    """One tab of the page, publishing until `close()`, which logs what it published."""

    def __init__(self, rendering: "_native.BrowserPageRendering", page_url: str) -> None:
        self._rendering = rendering
        self._page_url = page_url

    def close(self) -> None:
        rendering = self._rendering
        rendering.close()
        log.info(
            f"BrowserSource: {self._page_url} closed. frames_published={rendering.frames_published} "
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


@node.meta(
    description="Renders a web page offscreen with Chromium and publishes its frames, audio and events",
)
@node(execution="manual")
def BrowserSource(
    *,
    url: Input[
        Annotated[
            str,
            Description(
                "The page to render: an http(s), file or data URL, or a file path inside the "
                "project."
            ),
        ]
    ] = Input("about:blank"),
    width: Input[
        Annotated[
            int, NumberRange(minimum=1), Description("The page's width in pixels, and each frame's.")
        ]
    ] = Input(1920),
    height: Input[
        Annotated[
            int, NumberRange(minimum=1), Description("The page's height in pixels, and each frame's.")
        ]
    ] = Input(1080),
    frame_rate: Input[
        Annotated[
            int,
            NumberRange(minimum=1, maximum=60),
            Description("The most frames a second CEF paints; a still page paints none."),
        ]
    ] = Input(60),
    transparent_background: Input[
        Annotated[
            bool,
            Description(
                "Paint the page over transparency, so it composites as an overlay; "
                "otherwise over opaque white."
            ),
        ]
    ] = Input(True),
    audio_sample_rate: Input[
        Annotated[
            int, NumberRange(minimum=1), Description("The rate the page's audio is published at.")
        ]
    ] = Input(48_000),
    output_pool_rotation_depth: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description(
                "How many painted frames the output pool rotates before reusing a free slot."
            ),
        ]
    ] = Input(4),
    data: InputPin[
        Annotated[
            PageMessage,
            StreamDelivery("ordered"),
            Description("Each message is dispatched to the page as a `tatolab:data` event"),
        ]
    ],
    video_in_0: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description('A video the page takes as `window.tatolab.inputTrack("video_in_0")`'),
        ]
    ],
    video_in_1: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description('A video the page takes as `window.tatolab.inputTrack("video_in_1")`'),
        ]
    ],
    video_in_2: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description('A video the page takes as `window.tatolab.inputTrack("video_in_2")`'),
        ]
    ],
    video_in_3: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description('A video the page takes as `window.tatolab.inputTrack("video_in_3")`'),
        ]
    ],
    audio_in_0: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            Description('Audio the page takes as `window.tatolab.inputTrack("audio_in_0")`'),
        ]
    ],
    audio_in_1: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            Description('Audio the page takes as `window.tatolab.inputTrack("audio_in_1")`'),
        ]
    ],
    audio_in_2: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            Description('Audio the page takes as `window.tatolab.inputTrack("audio_in_2")`'),
        ]
    ],
    audio_in_3: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            Description('Audio the page takes as `window.tatolab.inputTrack("audio_in_3")`'),
        ]
    ],
    video: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(PAINTED_PAGE_TEXTURE_FORMAT),
            Description("The painted page, in sRGB"),
        ]
    ],
    audio: OutputPin[
        Annotated[
            Audio,
            Description("The page's audio, as 32-bit float samples; a silent page publishes none"),
        ]
    ],
    events: OutputPin[
        Annotated[
            PageMessage,
            Description("One message per `window.tatolab.publish(object)` call"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
    """A web page, published as RGBA pictures, audio and the objects it publishes."""
    page_input_pins = {
        "video_in_0": video_in_0,
        "video_in_1": video_in_1,
        "video_in_2": video_in_2,
        "video_in_3": video_in_3,
        "audio_in_0": audio_in_0,
        "audio_in_1": audio_in_1,
        "audio_in_2": audio_in_2,
        "audio_in_3": audio_in_3,
    }
    module_host_table = ctx.open_module_host_table(
        output_ports=[VIDEO_OUTPUT_PORT, EVENTS_OUTPUT_PORT, AUDIO_OUTPUT_PORT],
        input_ports=[DATA_INPUT_PORT, *VIDEO_INPUT_PORTS, *AUDIO_INPUT_PORTS],
    )

    def the_wired(ports: "tuple[str, ...]") -> "list[str]":
        return [port for port in ports if page_input_pins[port].inbound_link_names()]

    def open_the_page() -> PageOpenInTheBrowserHost:
        page_url = page_url_of(url.get())
        wired_video_input_ports = the_wired(VIDEO_INPUT_PORTS)
        wired_audio_input_ports = the_wired(AUDIO_INPUT_PORTS)
        log.info(
            f"BrowserSource: opening {page_url} at {width.get()}x{height.get()}, "
            f"up to {frame_rate.get()} fps, audio at {audio_sample_rate.get()} Hz, "
            f"inputs {wired_video_input_ports + wired_audio_input_ports}"
        )
        rendering = _native.BrowserPageRendering(
            module_host_table,
            VIDEO_OUTPUT_PORT,
            EVENTS_OUTPUT_PORT,
            AUDIO_OUTPUT_PORT,
            DATA_INPUT_PORT,
            wired_video_input_ports,
            wired_audio_input_ports,
            page_url,
            width.get(),
            height.get(),
            frame_rate.get(),
            transparent_background.get(),
            audio_sample_rate.get(),
            output_pool_rotation_depth.get(),
        )
        return PageOpenInTheBrowserHost(rendering, page_url)

    computed(open_the_page)
