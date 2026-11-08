# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `tatolab-moq`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest. The engine
never calls anything here.
"""

from collections.abc import Buffer
from typing import Any, final

# pyo3 builds this from what the module registers, so the stub carries it too.
__all__ = [
    "MoqGatewayThread",
    "MoqTrackDeclarationThread",
    "MoqTrackReceiveThread",
    "MoqTrackSendPublisher",
]

@final
class MoqGatewayThread:
    """A gateway's native half: the one relay session of its stream's MoQ nodes."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        runtime_id: str,
        stream_id: str,
        gateway_node: str,
        config_settings: dict[str, Any],
    ) -> MoqGatewayThread:
        """Start the gateway, dialing `config_settings` and each set of settings
        its `settings` input receives after."""

    def stop_and_join(self, timeout_seconds: float) -> bool:
        """Ask the thread to stop and wait up to `timeout_seconds`: `True` when it ended."""

@final
class MoqTrackDeclarationThread:
    """A track node's native half: its declaration on its gateway."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        runtime_id: str,
        stream_id: str,
        track_node: str,
        gateway_node: str,
        track: str,
        catalog_entry: dict[str, Any] | None = None,
    ) -> MoqTrackDeclarationThread:
        """Declare `track` on `gateway_node` for `track_node`, again every second."""

    def stop_and_join(self) -> None:
        """Stop declaring the track."""

@final
class MoqTrackSendPublisher:
    """A send node's native half: its writes to the track its config names."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        runtime_id: str,
        stream_id: str,
        track_node: str,
    ) -> MoqTrackSendPublisher:
        """Write to the track `track_node` declares."""

    def send_message(
        self,
        payload: Buffer,
        timestamp_ns: int,
        starts_group: bool,
        lost_since_last_read: int,
    ) -> None:
        """Hand the gateway one message; a payload that is not contiguous is refused."""

    def send_the_upstream_stream_facts_when_they_changed(
        self, upstream_stream_facts: dict[str, Any] | None = None
    ) -> None:
        """Hand the gateway the hang entry the upstream stream facts' wire form builds, when it changed."""

@final
class MoqTrackReceiveThread:
    """A receive node's native half: it asks its gateway for a track and
    writes what arrives onto `content`."""

    def __new__(
        cls,
        module_host_table: object,
        *,
        runtime_id: str,
        stream_id: str,
        receive_node: str,
        gateway_node: str,
        broadcast: str,
        track: str,
        max_age_milliseconds: int,
        family: str,
        codec: str | None = None,
    ) -> MoqTrackReceiveThread:
        """Ask `gateway_node` for `track` of `broadcast`; a `family` other than
        `packets` or `records` raises `ValueError`."""

    def stop_and_join(self, timeout_seconds: float) -> bool:
        """Ask the thread to stop and wait up to `timeout_seconds`: `True` when it ended."""
