# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""This wheel's native scene, stood in for by one that records each open and close, so the node
runs with no GPU."""

from __future__ import annotations

from pathlib import Path

WHAT_THE_SCENES_DID: "list[tuple[str, str, int, int]]" = []
"""Each `("opened" | "closed", riv_path, width, height)`, in order."""


class RiveSceneNotBuilt(RuntimeError):
    """Stands in for `_native.RiveSceneNotBuilt`."""


class RiveFileRefused(RiveSceneNotBuilt):
    """Stands in for `_native.RiveFileRefused`."""


class SceneRecordingItsOpenAndClose:
    """Stands in for `_native.RiveSceneRendering`: refuses a `.riv` file holding `refused`, as
    the native scene refuses a file rive-runtime cannot read."""

    def __init__(
        self,
        module_host_table: object,
        video_output_port: str,
        data_input_port: str,
        riv_path: str,
        artboard: "str | None",
        state_machine: "str | None",
        width: int,
        height: int,
        frame_rate: int,
        output_pool_rotation_depth: int,
    ) -> None:
        assert (video_output_port, data_input_port) == ("video", "data")
        if Path(riv_path).read_text() == "refused":
            raise RiveFileRefused(f"RiveSource: {riv_path!r}: not a .riv file")
        self.opened = (riv_path, width, height)
        WHAT_THE_SCENES_DID.append(("opened", *self.opened))

    def close(self) -> None:
        WHAT_THE_SCENES_DID.append(("closed", *self.opened))


RiveSceneRendering = SceneRecordingItsOpenAndClose
"""What `tatolab.rive._native` names the scene the node opens."""
