# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""MoQ gateway nodes for StreamLib: `MoqGateway`, `MoqTrack`, `MoqTrackSend`
and `MoqTrackReceive`.

A gateway holds one connection to a relay and publishes one broadcast, and gives
itself out on its `connection`; a track, wired that connection, declares one
outgoing track of the gateway's broadcast and gives itself out on its own
`connection`; a send, wired a track's connection, writes a port's messages to
that track; a receive, wired a gateway's connection, reads a track of any
broadcast at the relay its gateway dials. The nodes of one stream reach each
other over iceoryx2 shared memory, and only the gateway touches the network.
"""

from __future__ import annotations

import json
from collections.abc import Buffer, Mapping
from dataclasses import dataclass
from typing import Annotated, Any, Final, Literal, Self, TypeVar

from tatolab.stream import (
    Audio,
    Connection,
    Description,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    InputPin,
    NumberRange,
    OutputPin,
    PinTypeChoice,
    PinTypeFromSetting,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    SoleReferrer,
    StreamDelivery,
    Tensor,
    cast_exposed_name_to_url_safe,
    connection_type,
    log,
    named_pin_type,
    node,
)

from . import _native

__all__ = [
    "MoqCatalogEntry",
    "MoqConnection",
    "MoqGateway",
    "MoqRecord",
    "MoqRelaySettings",
    "MoqTrack",
    "MoqTrackConnection",
    "MoqTrackReceive",
    "MoqTrackSend",
]

GATEWAY_SETTINGS_INPUT_PORT = "settings"
RECEIVE_CONTENT_OUTPUT_PORT = "content"

#: How long `stop()` waits for a native thread. Each thread's own wait is
#: bounded well under it; a relay session closing takes the longest.
NATIVE_THREAD_JOIN_TIMEOUT_SECONDS = 5.0

# The tracks a gateway writes its broadcast's catalog on, which no `MoqTrack` takes.
GATEWAY_CATALOG_TRACK_NAMES = ("catalog.json", "catalog.json.z")

#: The settings a gateway dials from, each a field of its own and a key a `MoqRelaySettings` may set.
GATEWAY_RELAY_SETTING_NAMES: Final = (
    "kind",
    "broadcast",
    "relay_url",
    "token",
    "certificate_fingerprint",
    "project_id",
    "signing_key",
)

MoqGatewayRelayKind = Literal["moq-relay", "cloudflare", "moqpro"]
MoqCatalogSection = Literal["video", "audio", "text", "json", "binary"]

_SettingValue = TypeVar("_SettingValue")


@connection_type
class MoqConnection(Connection):
    """A `MoqGateway`: its one connection to a relay and the broadcast it publishes there."""


@connection_type
class MoqTrackConnection(Connection):
    """A `MoqTrack`: one outgoing track of its gateway's broadcast, which one `MoqTrackSend` writes."""


@named_pin_type(
    description=(
        "One JSON object a MoQ track carries as an object of its group, its UTF-8 JSON text "
        "the payload at the relay."
    )
)
@dataclass(frozen=True)
class MoqRecord:
    """One JSON object, travelling between nodes and through a relay as its UTF-8 JSON text."""

    members: Mapping[str, Any]
    """The object's members, each a value JSON holds: text, a number, a boolean, None, a list or
    another mapping."""

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        members = json.loads(bytes(payload))
        if not isinstance(members, dict):
            raise ValueError(f"a MoqRecord is one JSON object, not {type(members).__name__}")
        return cls(members)

    def to_bytes(self) -> bytes:
        return json.dumps(dict(self.members), separators=(",", ":"), allow_nan=False).encode()


@named_pin_type(
    description=(
        "Relay settings a MoQ gateway dials with, replacing its own: each of `kind`, "
        "`broadcast`, `relay_url`, `token`, `certificate_fingerprint`, `project_id` and "
        "`signing_key` it sets, `None` unsetting one."
    )
)
@dataclass(frozen=True)
class MoqRelaySettings:
    """The settings a node reading a secret store hands a gateway, as UTF-8 JSON text."""

    values: Mapping[str, str | None]
    """The settings it replaces, each a non-empty str, or `None` to unset it."""

    def __post_init__(self) -> None:
        for key, value in self.values.items():
            if key not in GATEWAY_RELAY_SETTING_NAMES:
                raise ValueError(
                    f"MoqRelaySettings: `{key}` is no gateway setting; they are "
                    f"{', '.join(f'`{name}`' for name in GATEWAY_RELAY_SETTING_NAMES)}"
                )
            _refuse_a_value_that_is_not_a_str("MoqRelaySettings", key, value)

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        values = json.loads(bytes(payload))
        if not isinstance(values, dict):
            raise ValueError(f"MoqRelaySettings are one JSON object, not {type(values).__name__}")
        return cls(values)

    def to_bytes(self) -> bytes:
        return json.dumps(dict(self.values), separators=(",", ":")).encode()


@dataclass(frozen=True)
class MoqCatalogEntry:
    """A track's entry in its broadcast's catalog, in hang's layout."""

    section: MoqCatalogSection
    """The section of the catalog the entry belongs in."""

    entry: str
    """The entry, as the JSON text of one object, such as `{"codec": "avc1.64001f"}`."""


def _refuse_a_value_that_is_not_a_str(node_type: str, key: str, value: object) -> None:
    if value is not None and (not isinstance(value, str) or not value):
        raise ValueError(f"{node_type}: `{key}` is {value!r}; it takes a non-empty str")


def _set_or_refused(node_type: str, key: str, value: "_SettingValue | None", what_it_takes: str) -> "_SettingValue":
    """`value`, or a refusal naming `key` when the node holds none."""
    if value is None:
        raise ValueError(f"{node_type}: `{key}` is not set; it takes {what_it_takes}")
    return value


def _catalog_entry_read_from(catalog: MoqCatalogEntry) -> "dict[str, Any]":
    """`catalog` as the native half takes it, `{"section": ..., "entry": {...}}`, or a refusal."""
    try:
        entry = json.loads(catalog.entry)
    except json.JSONDecodeError as not_json:
        raise ValueError(f"MoqTrack: `catalog`'s `entry` is no JSON text: {not_json}") from not_json
    if not isinstance(entry, dict):
        raise ValueError(f"MoqTrack: `catalog`'s `entry` is {entry!r}; it takes one JSON object")
    return {"section": catalog.section, "entry": entry}


@node.meta(
    description=(
        "Holds one connection to a MoQ relay and publishes one broadcast there.\n\n"
        "It gives itself out on `connection`: each `MoqTrack` wired it is a track of the "
        "broadcast, whose catalog merges their entries, and each `MoqTrackReceive` wired it "
        "reads any broadcast at the same relay over the same connection. The connection "
        "dials from its settings, and a `MoqRelaySettings` on `settings` replaces them — the "
        "way a node reading a secret store hands over a credential, `None` unsetting one. A "
        "relay that drops the connection is dialed again, and a moq.pro credential is "
        "renewed before it expires."
    ),
)
@node(execution="manual", paced_by_real_time=True)
class MoqGateway:
    """One relay connection, one broadcast."""

    kind: Annotated[
        MoqGatewayRelayKind | None,
        SettingOnly(),
        Description(
            "The relay's kind: `moq-relay` for one a person or a rig runs, `cloudflare` for "
            "one Cloudflare provisioned, `moqpro` for a moq.pro project."
        ),
    ] = None
    broadcast: Annotated[
        str | None,
        SettingOnly(),
        Description(
            "The broadcast's name, cast to URL-safe characters; the relay serves it as "
            "`<broadcast>.hang` under the credential's root."
        ),
    ] = None
    relay_url: Annotated[
        str | None,
        SettingOnly(),
        Description(
            "`moq-relay` and `cloudflare`: the relay's URL. Cloudflare's carries its token as "
            "the last path segment."
        ),
    ] = None
    token: Annotated[
        str | None, SettingOnly(), Description("`moq-relay`: a token its moq-auth server reads.")
    ] = None
    certificate_fingerprint: Annotated[
        str | None,
        SettingOnly(),
        Description(
            "`moq-relay`: the SHA-256 fingerprint of a self-signed certificate to pin, as the "
            "relay serves it at `/certificate.sha256`."
        ),
    ] = None
    project_id: Annotated[str | None, SettingOnly(), Description("`moqpro`: the project's id.")] = None
    signing_key: Annotated[
        str | None,
        SettingOnly(),
        Description("`moqpro`: the project's signing key, the JWK moq.pro gave."),
    ] = None
    settings: InputPin[
        Annotated[
            MoqRelaySettings | None,
            StreamDelivery("newest"),
            Description("Relay settings replacing the gateway's own; `None` unsets one"),
        ]
    ]
    connection: OutputPin[
        Annotated[MoqConnection, Description("This gateway, for a MoqTrack or a MoqTrackReceive")]
    ]

    def __init__(self) -> None:
        for key in GATEWAY_RELAY_SETTING_NAMES:
            if key == "kind":
                continue
            _refuse_a_value_that_is_not_a_str("MoqGateway", key, getattr(self, key))
        self._gateway_thread: "_native.MoqGatewayThread | None" = None

    def settings_stated(self) -> "dict[str, Any]":
        """The settings the gateway holds, as the native half reads them: each one set, its
        broadcast cast to URL-safe characters."""
        stated = {
            key: getattr(self, key) for key in GATEWAY_RELAY_SETTING_NAMES if getattr(self, key) is not None
        }
        if "broadcast" in stated:
            stated["broadcast"] = cast_exposed_name_to_url_safe(stated["broadcast"])
        return stated

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(input_ports=[GATEWAY_SETTINGS_INPUT_PORT])
        self._gateway_thread = _native.MoqGatewayThread(
            module_host_table,
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            gateway_node=ctx.node_name,
            config_settings=self.settings_stated(),
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


@node.meta(
    description=(
        "Declares one outgoing track of the broadcast of the `MoqGateway` wired into "
        "`gateway`.\n\n"
        "It gives itself out on `connection`, which one `MoqTrackSend` takes to write to the "
        "track; a second send wired the same track is refused at load. A stream may hold "
        "the connection in a graph variable. Its catalog entry is `catalog` when set and "
        "otherwise the one built from the stream facts of the port feeding its send."
    ),
)
@node(execution="manual")
class MoqTrack:
    """One track of a gateway's broadcast."""

    gateway: Annotated[
        MoqConnection | None,
        Description("The gateway whose broadcast carries the track; the node fails, naming `gateway`, until it is wired"),
    ] = None
    track: Annotated[
        str,
        SettingOnly(),
        Description(
            "The track's name in the broadcast, cast to URL-safe characters; the node fails, "
            "naming `track`, while it is empty."
        ),
    ] = ""
    catalog: Annotated[
        MoqCatalogEntry | None,
        SettingOnly(),
        Description(
            "The track's catalog entry, replacing the one built from the stream facts of the "
            "port feeding its send. Unset: the built entry."
        ),
    ] = None
    connection: OutputPin[Annotated[MoqTrackConnection, Description("This track, for one MoqTrackSend")]]

    def __init__(self) -> None:
        self._track_name = cast_exposed_name_to_url_safe(self.track) if self.track else ""
        if self._track_name in GATEWAY_CATALOG_TRACK_NAMES:
            raise ValueError(
                f"MoqTrack: `track` is {self._track_name!r}, a name the gateway writes its catalog on"
            )
        self._catalog_entry = None if self.catalog is None else _catalog_entry_read_from(self.catalog)
        self._declaration_thread: "_native.MoqTrackDeclarationThread | None" = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if not self._track_name:
            raise ValueError("MoqTrack: `track` is not set; it takes the track's name")
        _set_or_refused("MoqTrack", "gateway", self.gateway, "a `MoqGateway`'s connection")

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        self._declaration_thread = _native.MoqTrackDeclarationThread(
            ctx.open_module_host_table(),
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            track_node=ctx.node_name,
            gateway_node=_set_or_refused("MoqTrack", "gateway", self.gateway, "a `MoqGateway`'s connection").name,
            track=self._track_name,
            catalog_entry=self._catalog_entry,
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


#: What a `MoqTrackSend` writes to its track: encoded media, Audio, a CPU-resident Tensor, or a `MoqRecord`.
SentContent = EncodedVideo | EncodedAudio | Audio | Tensor | MoqRecord


@node.meta(
    description=(
        "Writes each message of `content` to the `MoqTrack` wired into `track`.\n\n"
        "Payload bytes go to the relay as written, stamped with the message's own stamp, "
        "inside hang's legacy container — the stamp as a QUIC varint in microseconds ahead "
        "of the bytes — when the track's catalog entry names that container, as an entry "
        "built from stream facts does; "
        "a message with `starts_group` set opens a new group at the relay, and the rest "
        "continue it. Under backpressure, or after a message reporting a loss, the rest "
        "of the group is dropped and the next group starts clean. The stream facts of "
        "the port feeding `content` build the track's catalog entry unless the track names "
        "its own. "
        "A Texture carries no bytes off its machine: encode it first."
    ),
)
@node
class MoqTrackSend:
    """A port's messages, out to a track."""

    track: Annotated[
        SoleReferrer[MoqTrackConnection] | None,
        Description("The track to write to; no other send takes it, and the node fails, naming `track`, until it is wired"),
    ] = None
    content: InputPin[
        Annotated[
            SentContent,
            StreamDelivery("ordered"),
            Description("The messages to send: encoded media, Audio, a CPU-resident Tensor or a MoqRecord"),
        ]
    ]

    def __init__(self) -> None:
        self._publisher: "_native.MoqTrackSendPublisher | None" = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        track = _set_or_refused("MoqTrackSend", "track", self.track, "a `MoqTrack`'s connection")
        self._publisher = _native.MoqTrackSendPublisher(
            ctx.open_module_host_table(),
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            track_node=track.name,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        publisher = self._publisher
        if publisher is None:
            return
        while (message := self.content.read_message_as_sent()) is not None:
            upstream_stream_facts = self.content.upstream_stream_facts(inbound_link=message.inbound_link)
            publisher.send_the_upstream_stream_facts_when_they_changed(
                None if upstream_stream_facts is None else upstream_stream_facts.to_wire()
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


#: The codecs a `MoqTrackReceive` reads encoded video in, and those it reads encoded audio in.
RECEIVED_VIDEO_CODECS: Final = ("h264", "h265")
RECEIVED_AUDIO_CODECS: Final = ("opus",)

#: What a `MoqTrackReceive` writes on `content`, as its `content_type` chooses: encoded media
#: of a codec the runtime decodes, or a `MoqRecord`.
ReceivedContent = (
    Annotated[EncodedVideo, EncodedCodecs("h264")]
    | Annotated[EncodedVideo, EncodedCodecs("h265")]
    | Annotated[EncodedAudio, EncodedCodecs("opus")]
    | MoqRecord
)


@node.meta(
    description=(
        "Reads one track of a broadcast at the relay of the `MoqGateway` wired into "
        "`gateway`.\n\n"
        "`content` carries each object's payload bytes as the relay delivered them — on a "
        "track whose catalog entry names hang's legacy container, the bytes after its "
        "timestamp, stamped at that timestamp — as the type `content_type` chooses, the "
        "first object of each group starting a group, and its stream facts are the ones the "
        "track's entry in the broadcast's catalog states. Groups arrive in sequence order; a "
        "missing group is waited for until newer content runs `max_age` past it in the "
        "track's own timestamps, then given up and counted in the reader's "
        "`lost_since_last_read`, and the read resumes at the next group start. Stamps are "
        "the track's own, re-based onto this machine's clock: an object running more "
        "than 100 ms ahead of its arrival lowers the offset, and a track stepping back or "
        "falling a second behind takes it again. A broadcast not yet published, or "
        "withdrawn, is asked for again and never ends the stream."
    ),
)
@node(execution="manual", paced_by_real_time=True)
class MoqTrackReceive:
    """A track at a relay, into the graph."""

    gateway: Annotated[
        MoqConnection | None,
        Description("The gateway whose connection reads the track; the node fails, naming `gateway`, until it is wired"),
    ] = None
    broadcast: Annotated[
        str,
        SettingOnly(),
        Description(
            "The broadcast's path at the relay, exactly as its publisher wrote it; a "
            "`MoqGateway`'s is `<broadcast>.hang`. The node fails, naming `broadcast`, while it "
            "is empty."
        ),
    ] = ""
    track: Annotated[
        str,
        SettingOnly(),
        Description(
            "The track of `broadcast`, exactly as its publisher named it; the node fails, "
            "naming `track`, while it is empty."
        ),
    ] = ""
    content_type: Annotated[
        PinTypeChoice,
        Description("What `content` carries: encoded video or audio in its codec, or a MoqRecord"),
    ] = PinTypeChoice(MoqRecord)
    max_age: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=0),
        Description(
            "How far newer content on the track may run past a missing group, in whole "
            "milliseconds of the track's own timestamps, before the group is given up. A "
            "transport budget, never a playout delay."
        ),
    ] = 100
    content: OutputPin[
        Annotated[
            ReceivedContent,
            PinTypeFromSetting("content_type"),
            Description("Each object of the track, out of hang's container when it rides one"),
        ]
    ]

    def __init__(self) -> None:
        chosen: Any = self.content_type.chosen
        self._carries = "named" if chosen["type"] == "named" else "encoded"
        self._codec: "str | None" = None if self._carries == "named" else chosen["codecs"][0]
        self._receive_thread: "_native.MoqTrackReceiveThread | None" = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        for key in ("broadcast", "track"):
            if not getattr(self, key):
                raise ValueError(f"MoqTrackReceive: `{key}` is not set; it takes the {key}'s name")
        _set_or_refused("MoqTrackReceive", "gateway", self.gateway, "a `MoqGateway`'s connection")

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        module_host_table = ctx.open_module_host_table(output_ports=[RECEIVE_CONTENT_OUTPUT_PORT])
        self._receive_thread = _native.MoqTrackReceiveThread(
            module_host_table,
            runtime_id=ctx.runtime_id,
            stream_id=ctx.stream_id,
            receive_node=ctx.node_name,
            gateway_node=_set_or_refused(
                "MoqTrackReceive", "gateway", self.gateway, "a `MoqGateway`'s connection"
            ).name,
            broadcast=self.broadcast,
            track=self.track,
            max_age_milliseconds=self.max_age,
            carries=self._carries,
            codec=self._codec,
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
