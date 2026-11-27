# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""This wheel's native page, stood in for by one that records each open and close, so the node
runs with no browser host."""

from __future__ import annotations

WHAT_THE_TABS_DID: "list[tuple[str, str, int, int, tuple[str, ...]]]" = []
"""Each `("opened" | "closed", url, width, height, wired inputs)`, in order."""

URL_THE_HOST_REFUSES = "https://refused.example/"


class PageRecordingItsOpenAndClose:
    """Stands in for `_native.BrowserPageRendering`: refuses `URL_THE_HOST_REFUSES`, as the
    browser host refuses a tab it cannot open."""

    frames_published = frames_published_from_painted_textures = frames_dropped = 0
    audio_blocks_published = events_published = data_messages_delivered = 0
    data_messages_refused = page_input_video_frames_delivered = 0
    page_input_audio_blocks_delivered = page_input_chunks_refused = browser_host_reconnects = 0

    def __init__(
        self,
        module_host_table: object,
        video_output_port: str,
        events_output_port: str,
        audio_output_port: str,
        data_input_port: str,
        video_input_ports: "list[str]",
        audio_input_ports: "list[str]",
        url: str,
        width: int,
        height: int,
        frame_rate: int,
        transparent_background: bool,
        audio_sample_rate: int,
        output_pool_rotation_depth: int,
    ) -> None:
        if url == URL_THE_HOST_REFUSES:
            raise RuntimeError(f"the browser host did not open {url}")
        self.opened = (url, width, height, (*video_input_ports, *audio_input_ports))
        WHAT_THE_TABS_DID.append(("opened", *self.opened))

    def close(self) -> None:
        WHAT_THE_TABS_DID.append(("closed", *self.opened))


BrowserPageRendering = PageRecordingItsOpenAndClose
"""What `tatolab.browser._native` names the page the node opens."""
