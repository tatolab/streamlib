# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-browser`, as the wheel's Python sees it."""

from typing import Final, final

__all__ = ["BROWSER_SUBPROCESS_EXECUTABLE_NAME", "BrowserPageRendering"]

BROWSER_SUBPROCESS_EXECUTABLE_NAME: Final[str]

@final
class BrowserPageRendering:
    """A web page rendering offscreen in this helper, publishing its frames on
    `video`, its audio on `audio` and its published objects on `events`, and
    handed every `data` message as a DOM event."""

    def __new__(
        cls,
        module_host_table: object,
        cef_runtime_directory: str,
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
        """Initialize CEF from `cef_runtime_directory` if this helper has not, and open `url`.

        `module_host_table` is what `ctx.open_module_host_table(...)` returns with the three
        output ports and the input port lent.
        """
    @property
    def frames_published(self) -> int: ...
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
    def close(self) -> None:
        """Stop delivering data and close the page. Idempotent."""
