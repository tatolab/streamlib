# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""MoQ gateway nodes for StreamLib: `MoqGateway`, `MoqTrack`, `MoqTrackSend`
and `MoqTrackReceive`.

A gateway holds one connection to a relay and publishes one broadcast; a track
declares one outgoing track of a gateway's broadcast; a send writes a port's
messages to a track it names by reference; a receive reads a track of any
broadcast at the relay its gateway dials. The nodes of one stream reach each
other over iceoryx2 shared memory, and only the gateway touches the network.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated, Any, Literal

from tatolab.stream import (
    Array,
    Channels,
    Input,
    NodeReferenceToNodeType,
    Output,
    Packets,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SoleReferrerNodeReferenceToNodeType,
    cast_exposed_name_to_url_safe,
    from_config,
    log,
    node,
)

from . import _native

__all__ = [
    "MoqGateway",
    "MoqGatewayConfig",
    "MoqTrack",
    "MoqTrackConfig",
    "MoqTrackReceive",
    "MoqTrackReceiveConfig",
    "MoqTrackSend",
    "MoqTrackSendConfig",
]

GATEWAY_SETTINGS_INPUT_PORT = "settings"
SEND_CONTENT_INPUT_PORT = "content"
RECEIVE_CONTENT_OUTPUT_PORT = "content"

#: How long `stop()` waits for a native thread. Each thread's own wait is
#: bounded well under it; a relay session closing takes the longest.
NATIVE_THREAD_JOIN_TIMEOUT_SECONDS = 5.0

#: The sections of a broadcast's catalog a track's entry belongs in.
CATALOG_SECTIONS = ("video", "audio", "text", "json", "binary")

# The tracks a gateway writes its broadcast's catalog on, which no `MoqTrack` takes.
GATEWAY_CATALOG_TRACK_NAMES = ("catalog.json", "catalog.json.z")

MoqGatewayRelayKind = Literal["moq-relay", "cloudflare", "moqpro"]
MoqTrackReceiveFamily = Literal["packets", "records"]


def _refuse_a_value_that_is_not_a_str(node_type: str, key: str, value: object) -> None:
    if value is not None and (not isinstance(value, str) or not value):
        raise ValueError(f"{node_type}: `{key}` is {value!r}; it takes a non-empty str")


@dataclasses.dataclass
class MoqGatewayConfig:
    """What a `MoqGateway` dials and publishes.

    Every key may be left to the `settings` input, whose values replace the
    config's; the gateway dials once the two together name a relay.
    """

    kind: Annotated[
        "MoqGatewayRelayKind | None",
        "The relay's kind: `moq-relay` for one a person or a rig runs, `cloudflare` for "
        "one Cloudflare provisioned, `moqpro` for a moq.pro project.",
    ] = None
    broadcast: Annotated[
        "str | None",
        "The broadcast's name, cast to URL-safe characters; the relay serves it as "
        "`<broadcast>.hang` under the credential's root.",
    ] = None
    relay_url: Annotated[
        "str | None",
        "`moq-relay` and `cloudflare`: the relay's URL. Cloudflare's carries its token "
        "as the last path segment.",
    ] = None
    token: Annotated["str | None", "`moq-relay`: a token its moq-auth server reads."] = None
    certificate_fingerprint: Annotated[
        "str | None",
        "`moq-relay`: the SHA-256 fingerprint of a self-signed certificate to pin, as the "
        "relay serves it at `/certificate.sha256`.",
    ] = None
    project_id: Annotated["str | None", "`moqpro`: the project's id."] = None
    signing_key: Annotated[
        "str | None", "`moqpro`: the project's signing key, the JWK moq.pro gave."
    ] = None

    def __post_init__(self) -> None:
        if self.kind is not None and self.kind not in ("moq-relay", "cloudflare", "moqpro"):
            raise ValueError(
                f"MoqGateway: `kind` is {self.kind!r}; it is `moq-relay`, `cloudflare` or `moqpro`"
            )
        for key in ("broadcast", "relay_url", "token", "certificate_fingerprint", "project_id", "signing_key"):
            _refuse_a_value_that_is_not_a_str("MoqGateway", key, getattr(self, key))
        if self.broadcast is not None:
            self.broadcast = cast_exposed_name_to_url_safe(self.broadcast)

    def settings_stated(self) -> "dict[str, Any]":
        """The keys the config sets, as the native half reads them."""
        return {
            field.name: getattr(self, field.name)
            for field in dataclasses.fields(self)
            if getattr(self, field.name) is not None
        }


@node.process(
    execution="manual",
    paced_by_real_time=True,
    description=(
        "Holds one connection to a MoQ relay and publishes one broadcast there.\n\n"
        "`MoqTrack` nodes naming this gateway are the broadcast's tracks, and the "
        "broadcast's catalog merges their entries. `MoqTrackReceive` nodes naming it read "
        "any broadcast at the same relay over the same connection. The connection dials "
        "from the config, and a Records message on `settings` replaces the config's "
        "values — the way a node reading a secret store hands over a credential, `null` "
        "unsetting a key. A relay that drops the connection is dialed again, and a "
        "moq.pro credential is renewed before it expires."
    ),
)
class MoqGateway:
    """One relay connection, one broadcast."""

    def __init__(self, config: MoqGatewayConfig) -> None:
        self._config = config
        self._gateway_thread: "_native.MoqGatewayThread | None" = None

    settings = Input(
        Records,
        delivery="newest",
        description="Relay settings whose values replace the config's; `null` unsets a key",
    )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(input_ports=[GATEWAY_SETTINGS_INPUT_PORT])
        self._gateway_thread = _native.MoqGatewayThread(
            module_host_table,
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            gateway_node=ctx.node_name,
            config_settings=self._config.settings_stated(),
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_gateway_thread()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_gateway_thread()

    def _stop_the_gateway_thread(self) -> None:
        if self._gateway_thread is None:
            return
        if not self._gateway_thread.stop_and_join(NATIVE_THREAD_JOIN_TIMEOUT_SECONDS):
            log.warn("MoqGateway: the relay session is still closing")
        self._gateway_thread = None


@dataclasses.dataclass
class MoqTrackConfig:
    """Which gateway's broadcast a `MoqTrack` is a track of, and its name there."""

    gateway: Annotated[
        "NodeReferenceToNodeType[MoqGateway]", "The gateway whose broadcast carries the track."
    ]
    track: Annotated["str", "The track's name in the broadcast, cast to URL-safe characters."]
    catalog: Annotated[
        "dict[str, Any] | None",
        "The track's catalog entry, `{\"section\": ..., \"entry\": {...}}` in hang's layout, "
        "replacing the entry of the port feeding its send. Absent: that port's entry.",
    ] = None

    def __post_init__(self) -> None:
        if not isinstance(self.track, str) or not self.track:
            raise ValueError(f"MoqTrack: `track` is {self.track!r}; it takes a non-empty str")
        self.track = cast_exposed_name_to_url_safe(self.track)
        if self.track in GATEWAY_CATALOG_TRACK_NAMES:
            raise ValueError(
                f"MoqTrack: `track` is {self.track!r}, a name the gateway writes its catalog on"
            )
        if self.catalog is not None and (
            not isinstance(self.catalog, dict)
            or self.catalog.get("section") not in CATALOG_SECTIONS
            or not isinstance(self.catalog.get("entry"), dict)
        ):
            raise ValueError(
                f"MoqTrack: `catalog` is {self.catalog!r}; it takes "
                f"`{{\"section\": ..., \"entry\": {{...}}}}` with a section of "
                f"{', '.join(CATALOG_SECTIONS)}"
            )


@node.process(
    execution="manual",
    description=(
        "Declares one outgoing track of a `MoqGateway`'s broadcast.\n\n"
        "A stream holds it as a graph variable, and one `MoqTrackSend` writes to it by "
        "reference; a second send naming the same track is refused at load. Its catalog "
        "entry is `catalog` when set and the entry of the port feeding its send "
        "otherwise. It has no ports."
    ),
)
class MoqTrack:
    """One track of a gateway's broadcast."""

    def __init__(self, config: MoqTrackConfig) -> None:
        self._config = config
        self._declaration_thread: "_native.MoqTrackDeclarationThread | None" = None

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        self._declaration_thread = _native.MoqTrackDeclarationThread(
            ctx.open_module_host_table(),
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            track_node=ctx.node_name,
            gateway_node=self._config.gateway.name,
            track=self._config.track,
            catalog_entry=self._config.catalog,
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_declaring()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_declaring()

    def _stop_declaring(self) -> None:
        if self._declaration_thread is None:
            return
        self._declaration_thread.stop_and_join()
        self._declaration_thread = None


@dataclasses.dataclass
class MoqTrackSendConfig:
    """Which track a `MoqTrackSend` writes to."""

    track: Annotated[
        "SoleReferrerNodeReferenceToNodeType[MoqTrack]",
        "The track to write to; no other send names it.",
    ]


@node.process(
    description=(
        "Writes each message of `content` to the `MoqTrack` it names.\n\n"
        "Payload bytes go to the relay as written, stamped with the message's own stamp; "
        "a message with `starts_group` set opens a new group at the relay, and the rest "
        "continue it. Under backpressure, or after a message reporting a loss, the rest "
        "of the group is dropped and the next group starts clean. The entry of the port "
        "feeding `content` is the track's catalog entry unless the track names its own. "
        "An Image carries no bytes off its machine: encode it first."
    ),
)
class MoqTrackSend:
    """A port's messages, out to a track."""

    def __init__(self, config: MoqTrackSendConfig) -> None:
        self._config = config
        self._publisher: "_native.MoqTrackSendPublisher | None" = None

    content = Input(
        Packets,
        also_takes=(Records, Channels, Array),
        delivery="ordered",
        description="The messages to send, Packets, Records, Channels or CPU-resident Array",
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._publisher = _native.MoqTrackSendPublisher(
            ctx.open_module_host_table(),
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            track_node=self._config.track.name,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        publisher = self._publisher
        if publisher is None:
            return
        while (message := ctx.inputs.read_message(SEND_CONTENT_INPUT_PORT)) is not None:
            publisher.send_the_upstream_catalog_entry_when_it_changed(
                ctx.inputs.inbound_link_catalog_entry(
                    SEND_CONTENT_INPUT_PORT, inbound_link=message.inbound_link
                )
            )
            publisher.send_message(
                message.payload,
                message.timestamp_ns,
                message.starts_group,
                message.lost_since_last_read,
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._publisher = None


@dataclasses.dataclass
class MoqTrackReceiveConfig:
    """Which track a `MoqTrackReceive` reads, and what family it writes it as."""

    gateway: Annotated[
        "NodeReferenceToNodeType[MoqGateway]", "The gateway whose connection reads the track."
    ]
    broadcast: Annotated[
        "str",
        "The broadcast's path at the relay, exactly as its publisher wrote it; a "
        "`MoqGateway`'s is `<broadcast>.hang`.",
    ]
    track: Annotated["str", "The track of `broadcast`, exactly as its publisher named it."]
    family: Annotated[
        "MoqTrackReceiveFamily",
        "The family `content` carries: `packets` for encoded media, `records` for records.",
    ]
    codec: Annotated[
        "str | None",
        "`packets`: the codec the track's media frames are encoded with, e.g. `h264`.",
    ] = None
    max_age: Annotated[
        "int",
        "How far newer content on the track may run past a missing group, in whole "
        "milliseconds of the track's own timestamps, before the group is given up. A "
        "transport budget, never a playout delay.",
    ] = 100

    def __post_init__(self) -> None:
        for key in ("broadcast", "track"):
            _refuse_a_value_that_is_not_a_str("MoqTrackReceive", key, getattr(self, key))
        if self.family not in ("packets", "records"):
            raise ValueError(
                f"MoqTrackReceive: `family` is {self.family!r}; it carries `packets` or `records`"
            )
        if self.family == "packets":
            _refuse_a_value_that_is_not_a_str("MoqTrackReceive", "codec", self.codec)
            if self.codec is None:
                raise ValueError("MoqTrackReceive: a `packets` track names its `codec`")
        elif self.codec is not None:
            raise ValueError("MoqTrackReceive: `codec` names a `packets` track's codec only")
        if isinstance(self.max_age, bool) or not isinstance(self.max_age, int) or self.max_age < 0:
            raise ValueError(
                f"MoqTrackReceive: `max_age` is {self.max_age!r}; it takes whole milliseconds, 0 or more"
            )


@node.process(
    execution="manual",
    paced_by_real_time=True,
    description=(
        "Reads one track of a broadcast at the relay its `MoqGateway` dials.\n\n"
        "`content` carries each object's payload bytes as the relay delivered them, the "
        "first object of each group starting a group, and its catalog entry is the "
        "track's own from the broadcast's catalog. Groups arrive in sequence order; a "
        "missing group is waited for until newer content runs `max_age` past it in the "
        "track's own timestamps, then given up and counted in the reader's "
        "`lost_since_last_read`, and the read resumes at the next group start. Stamps are "
        "the track's own, re-based onto this machine's clock: an object running more "
        "than 100 ms ahead of its arrival lowers the offset, and a track stepping back or "
        "falling a second behind takes it again. A broadcast not yet published, or "
        "withdrawn, is asked for again and never ends the stream."
    ),
)
class MoqTrackReceive:
    """A track at a relay, into the graph."""

    def __init__(self, config: MoqTrackReceiveConfig) -> None:
        self._config = config
        self._receive_thread: "_native.MoqTrackReceiveThread | None" = None

    content = Output(
        Packets,
        family_from=from_config("family"),
        codec=from_config("codec"),
        description="Each object of the track, as the relay delivered it",
    )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(output_ports=[RECEIVE_CONTENT_OUTPUT_PORT])
        self._receive_thread = _native.MoqTrackReceiveThread(
            module_host_table,
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            receive_node=ctx.node_name,
            gateway_node=self._config.gateway.name,
            broadcast=self._config.broadcast,
            track=self._config.track,
            max_age_milliseconds=self._config.max_age,
            family=self._config.family,
            codec=self._config.codec,
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_receive_thread()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_receive_thread()

    def _stop_the_receive_thread(self) -> None:
        if self._receive_thread is None:
            return
        if not self._receive_thread.stop_and_join(NATIVE_THREAD_JOIN_TIMEOUT_SECONDS):
            log.warn("MoqTrackReceive: the receive thread is still finishing")
        self._receive_thread = None
