# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-rive`, as the wheel's Python sees it."""

from typing import final

__all__ = ["RiveSceneRendering"]

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
        """Read `riv_path` and start the frame thread.

        `module_host_table` is what `ctx.open_module_host_table(output_ports=[video_output_port],
        input_ports=[data_input_port])` returns.
        """
    def stop(self) -> None:
        """Stop rendering and release the device. Idempotent."""
