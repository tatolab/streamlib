# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-browser`, as the wheel's Python sees it."""

from typing import final

__all__ = ["BrowserPageRendering"]

@final
class BrowserPageRendering:
    """A web page rendering as a tab in the machine's browser host, publishing its frames on
    `video`, its audio on `audio` and its published objects on `events`, and
    handed every `data` message as a DOM event."""

    def __new__(
        cls,
        module_host_table: object,
        video_output_port: str,
        events_output_port: str,
        audio_output_port: str,
        data_input_port: str,
        url: str,
        width: int,
        height: int,
        frame_rate: int,
        transparent_background: bool,
        audio_sample_rate: int,
        output_pool_rotation_depth: int,
    ) -> BrowserPageRendering:
        """Lease the machine's browser host and open `url` as a tab in it.

        `module_host_table` is what `ctx.open_module_host_table(...)` returns with the three
        output ports and the input port lent.
        """
    @property
    def frames_published(self) -> int: ...
    @property
    def frames_published_from_painted_textures(self) -> int:
        """Of `frames_published`, those copied on the GPU from a texture the page painted into."""
    @property
    def frames_dropped(self) -> int: ...
    @property
    def audio_blocks_published(self) -> int: ...
    @property
    def events_published(self) -> int: ...
    @property
    def data_messages_delivered(self) -> int: ...
    @property
    def data_messages_refused(self) -> int: ...
    @property
    def browser_host_reconnects(self) -> int: ...
    def close(self) -> None:
        """Stop delivering data and close the tab. Idempotent."""
