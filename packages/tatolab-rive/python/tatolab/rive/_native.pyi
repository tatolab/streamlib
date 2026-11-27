# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The native half of `tatolab-rive`, as the wheel's Python sees it."""

from typing import final

__all__ = [
    "RiveDeviceRefused",
    "RiveFileRefused",
    "RiveFileUnreadable",
    "RiveFrameThreadFailed",
    "RivePortRefused",
    "RiveSceneNotBuilt",
    "RiveSceneRendering",
    "RiveSizeNotPositive",
    "RiveSurfaceShareUnreachable",
]

class RiveSceneNotBuilt(RuntimeError):
    """A scene could not be built; each reason is its own subclass."""

class RiveSizeNotPositive(RiveSceneNotBuilt):
    """`width`, `height` or `frame_rate` is 0."""

class RiveFileUnreadable(RiveSceneNotBuilt):
    """The `.riv` file could not be read."""

class RiveFileRefused(RiveSceneNotBuilt):
    """rive-runtime refused the file, or it holds no artboard or state machine of the name given."""

class RivePortRefused(RiveSceneNotBuilt):
    """The `video` or `data` port the node lent could not be opened."""

class RiveDeviceRefused(RiveSceneNotBuilt):
    """No external-renderer device could be created on the runtime's GPU."""

class RiveSurfaceShareUnreachable(RiveSceneNotBuilt):
    """The surface share that output textures are checked out through is unreachable."""

class RiveFrameThreadFailed(RiveSceneNotBuilt):
    """The frame thread could not start, or ended before its scene was built."""

@final
class RiveSceneRendering:
    """A `.riv` scene publishing Image frames on a lent output port from a thread this wheel owns."""

    def __new__(
        cls,
        module_host_table: object,
        video_output_port: str,
        data_input_port: str,
        riv_path: str,
        artboard: str | None,
        state_machine: str | None,
        width: int,
        height: int,
        frame_rate: int,
        output_pool_rotation_depth: int,
    ) -> RiveSceneRendering:
        """Read `riv_path`, start the frame thread and return once its scene is built, raising
        the subclass of `RiveSceneNotBuilt` that says why it could not be.

        `module_host_table` is what `ctx.open_module_host_table(output_ports=[video_output_port],
        input_ports=[data_input_port])` returns.
        """
    def close(self) -> None:
        """Stop rendering and release the device. Idempotent."""
