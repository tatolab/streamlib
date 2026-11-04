# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The v4l2loopback module's control node: whether this user may create cameras, and creating, finding and removing them."""

from __future__ import annotations

import enum
import errno
import fcntl
import os
from typing import Callable, Protocol, TypeVar

from tatolab.stream import log

from .v4l2_output_device_abi import (
    V4L2LOOPBACK_CARD_LABEL_BYTES,
    V4L2LOOPBACK_CTL_ADD,
    V4L2LOOPBACK_CTL_QUERY,
    V4L2LOOPBACK_CTL_REMOVE,
    V4L2LOOPBACK_CTL_VERSION,
    V4l2LoopbackConfig,
)

V4L2LOOPBACK_CONTROL_NODE_PATH = "/dev/v4l2loopback"

ENABLE_VIRTUAL_CAMERA_VERB = "tatolab enable-virtual-camera"
"""The one-time command that grants this machine's user the control node."""

LOOPBACK_DEVICE_BUFFER_COUNT = 4

_SetupResult = TypeVar("_SetupResult")


class ControlNodeAccess(enum.Enum):
    """What this user may do with the control node."""

    WRITABLE = "writable"
    ABSENT = "absent"
    NOT_WRITABLE = "not writable by this user"


class V4l2LoopbackControlNode(Protocol):
    """What a sink needs from the module's control node."""

    def access(self) -> ControlNodeAccess: ...

    def candidate_device_numbers(self) -> "list[int]": ...

    def query_device(self, device_number: int) -> "V4l2LoopbackConfig | None": ...

    def add_device(self, config: V4l2LoopbackConfig) -> int: ...

    def remove_device(self, device_number: int) -> None: ...

    def module_version(self) -> "str | None": ...


def loopback_config_for_a_new_camera(label: str) -> V4l2LoopbackConfig:
    """`CTL_ADD`'s argument: the next free device number, this label, and the module's defaults elsewhere."""
    config = V4l2LoopbackConfig()
    config.output_nr = -1
    config.card_label = label.encode("utf-8")[: V4L2LOOPBACK_CARD_LABEL_BYTES - 1]
    config.max_buffers = LOOPBACK_DEVICE_BUFFER_COUNT
    config.announce_all_caps = 0
    return config


class V4l2LoopbackControlNodeOnDisk:
    """The control node at `/dev/v4l2loopback`, opened for each request."""

    def __init__(self, control_node_path: str = V4L2LOOPBACK_CONTROL_NODE_PATH) -> None:
        self._control_node_path = control_node_path

    def access(self) -> ControlNodeAccess:
        try:
            os.close(os.open(self._control_node_path, os.O_RDWR))
        except FileNotFoundError:
            return ControlNodeAccess.ABSENT
        except OSError:
            return ControlNodeAccess.NOT_WRITABLE
        return ControlNodeAccess.WRITABLE

    def candidate_device_numbers(self) -> "list[int]":
        try:
            names = os.listdir("/dev")
        except OSError:
            return []
        return sorted(
            int(name[len("video") :])
            for name in names
            if name.startswith("video") and name[len("video") :].isdigit()
        )

    def query_device(self, device_number: int) -> "V4l2LoopbackConfig | None":
        config = V4l2LoopbackConfig()
        config.output_nr = device_number
        try:
            self._control_ioctl(V4L2LOOPBACK_CTL_QUERY, config)
        except OSError:
            return None
        return config

    def add_device(self, config: V4l2LoopbackConfig) -> int:
        return self._control_ioctl(V4L2LOOPBACK_CTL_ADD, config)

    def remove_device(self, device_number: int) -> None:
        self._control_ioctl(V4L2LOOPBACK_CTL_REMOVE, device_number)

    def module_version(self) -> "str | None":
        version_code = bytearray(4)
        try:
            self._control_ioctl(V4L2LOOPBACK_CTL_VERSION, version_code)
        except OSError:
            return None
        code = int.from_bytes(version_code, "little")
        return f"{(code >> 16) & 0xFF}.{(code >> 8) & 0xFF}.{code & 0xFF}"

    def _control_ioctl(
        self, request: int, argument: "V4l2LoopbackConfig | bytearray | int"
    ) -> int:
        control_node_fd = os.open(self._control_node_path, os.O_RDWR)
        try:
            return fcntl.ioctl(control_node_fd, request, argument)
        finally:
            os.close(control_node_fd)


def find_device_carrying_label(
    control_node: V4l2LoopbackControlNode, label: str
) -> "int | None":
    """The loopback device already carrying `label`, left by a crash or by a reader that held it at the last teardown."""
    for device_number in control_node.candidate_device_numbers():
        config = control_node.query_device(device_number)
        if config is not None and config.label() == label:
            return device_number
    return None


def create_or_reclaim_device_then(
    control_node: V4l2LoopbackControlNode,
    label: str,
    then: "Callable[[int, bool], _SetupResult]",
) -> _SetupResult:
    """Create the camera's device, or reclaim the one carrying `label`, then run `then(device_number, reclaimed)`.

    A device this call created is removed again if `then` raises: nothing
    holds it yet, and a phantom camera in every picker is worse than none.
    """
    device_number = find_device_carrying_label(control_node, label)
    reclaimed = device_number is not None
    if device_number is None:
        try:
            device_number = control_node.add_device(
                loopback_config_for_a_new_camera(label)
            )
        except OSError as refusal:
            raise RuntimeError(
                f'VirtualCameraSink "{label}": the module refused to create a camera '
                f"(CTL_ADD on {V4L2LOOPBACK_CONTROL_NODE_PATH}): {refusal}"
            ) from refusal
    try:
        return then(device_number, reclaimed)
    except BaseException:
        if not reclaimed:
            try:
                control_node.remove_device(device_number)
            except OSError as remove_failure:
                log.warn(
                    "VirtualCameraSink: the device created by a setup that then failed could "
                    "not be removed; the next setup reclaims it by label",
                    camera=label,
                    device_number=device_number,
                    error=str(remove_failure),
                )
        raise


def removal_was_refused_because_a_reader_holds_the_device(refusal: OSError) -> bool:
    """Whether `CTL_REMOVE` refused because a reader still holds the camera open."""
    return refusal.errno == errno.EBUSY
