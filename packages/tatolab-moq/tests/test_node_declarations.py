# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The four nodes as a stream names them: their types, ports, connections and
the settings each refuses. Nothing here needs a runtime or a relay; the native
half's round trip through a relay is the crate's own test."""

from typing import Annotated, Any

import pytest
from tatolab.moq import (
    MoqCatalogEntry,
    MoqConnection,
    MoqGateway,
    MoqRecord,
    MoqRelaySettings,
    MoqTrack,
    MoqTrackReceive,
    MoqTrackSend,
)
from tatolab.stream import (
    EncodedCodecs,
    EncodedVideo,
    GetVariable,
    PinTypeChoice,
    SetVariable,
    StreamBuilder,
    graph_generator,
    run_graph_generator,
)
from tatolab.stream._class_node_construction import class_node_members, construct_class_node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

MOQ_RECORD = {"type": "named", "named_pin_type": "tatolab.moq:MoqRecord"}
H264 = {"type": "encoded_video", "codecs": ["h264"]}


@graph_generator
def a_track_sent_through_a_variable_and_read_back(stream_builder: StreamBuilder) -> None:
    gateway = stream_builder.add(
        MoqGateway,
        name="gateway",
        input_values={"kind": "moq-relay", "relay_url": "https://localhost:4443", "broadcast": "Studio A"},
    )
    camera_track = stream_builder.add(MoqTrack, name="camera-track", input_values={"track": "camera"})
    stream_builder.connect(gateway.output("connection"), camera_track.input("gateway"))
    setter = stream_builder.add(SetVariable, input_values={"variable": "camera"})
    stream_builder.connect(camera_track.output("connection"), setter.input("value"))
    getter = stream_builder.add(GetVariable, name="get-camera-track", input_values={"variable": "camera"})
    send = stream_builder.add(MoqTrackSend, name="send")
    stream_builder.connect(getter.output("value"), send.input("track"))
    receive = stream_builder.add(
        MoqTrackReceive,
        name="receive",
        input_values={
            "broadcast": "studio-a.hang",
            "track": "camera",
            "content_type": PinTypeChoice(Annotated[EncodedVideo, EncodedCodecs("h264")]),
        },
    )
    stream_builder.connect(gateway.output("connection"), receive.input("gateway"))


def the_port(node_class: type, ports_stamp: str, port_name: str) -> "dict[str, Any]":
    (port,) = [port for port in getattr(node_class, ports_stamp) if port["name"] == port_name]
    return port


def test_each_node_is_named_by_its_import_path_and_wired_its_connections():
    graph = run_graph_generator(a_track_sent_through_a_variable_and_read_back)
    types = {node["name"]: node["type"] for node in graph["nodes"]}
    assert types["gateway"] == "tatolab.moq:MoqGateway"
    assert types["camera-track"] == "tatolab.moq:MoqTrack"
    assert types["send"] == "tatolab.moq:MoqTrackSend"
    assert types["receive"] == "tatolab.moq:MoqTrackReceive"
    receive = next(node for node in graph["nodes"] if node["name"] == "receive")
    assert receive["input_values"]["content_type"] == H264
    links = {
        (link["source"]["node"], link["target"]["node"], link["target"]["port"]) for link in graph["links"]
    }
    assert ("gateway", "camera-track", "gateway") in links
    assert ("gateway", "receive", "gateway") in links
    assert ("get-camera-track", "send", "track") in links


def test_a_gateway_and_a_track_give_themselves_out_and_a_send_is_the_one_taker_of_its_track():
    gateway_out = the_port(MoqGateway, NODE_DECLARATION_OUTPUT_PORTS_STAMP, "connection")
    track_out = the_port(MoqTrack, NODE_DECLARATION_OUTPUT_PORTS_STAMP, "connection")
    assert gateway_out["pin_declaration"]["pin_type"]["pin_type"]["type"] == "connection"
    assert track_out["pin_declaration"]["pin_type"]["pin_type"]["type"] == "connection"
    for node_class in (MoqTrack, MoqTrackReceive):
        gateway_in = the_port(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, "gateway")
        assert gateway_in["pin_declaration"]["pin_type"]["pin_type"]["connection_type"] == (
            "tatolab.moq:MoqConnection"
        )
    track_in = the_port(MoqTrackSend, NODE_DECLARATION_INPUT_PORTS_STAMP, "track")
    assert track_in["pin_declaration"]["pin_type"]["pin_type"]["connection_type"] == (
        "tatolab.moq:MoqTrackConnection"
    )
    assert track_in["pin_declaration"]["pin_type"]["pin_type"]["one_referrer_per_referenced_node"] is True


def test_a_gateway_takes_relay_settings_and_a_send_takes_media_audio_tensors_and_records():
    settings = the_port(MoqGateway, NODE_DECLARATION_INPUT_PORTS_STAMP, "settings")
    content_in = the_port(MoqTrackSend, NODE_DECLARATION_INPUT_PORTS_STAMP, "content")
    assert settings["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "named",
        "named_pin_type": "tatolab.moq:MoqRelaySettings",
    }
    assert [each["type"] for each in content_in["pin_declaration"]["pin_type"]["pin_types"]] == [
        "encoded_video",
        "encoded_audio",
        "audio",
        "tensor",
        "named",
    ]


def test_a_receive_writes_the_type_its_choice_holds_a_moq_record_by_default():
    content_type = the_port(MoqTrackReceive, NODE_DECLARATION_INPUT_PORTS_STAMP, "content_type")
    content_out = the_port(MoqTrackReceive, NODE_DECLARATION_OUTPUT_PORTS_STAMP, "content")
    assert content_type["pin_default"] == MOQ_RECORD
    assert content_type["pin_declaration"]["pin_type"]["pin_type"]["among"] == [
        H264,
        {"type": "encoded_video", "codecs": ["h265"]},
        {"type": "encoded_audio", "codecs": ["opus"]},
        MOQ_RECORD,
    ]
    assert content_out["family"] == {
        "family": "records",
        "family_from_config_key": "content_type",
        "codec_from_config_key": "content_type",
    }


def a_node_built(node_class: type, **field_values: Any) -> Any:
    members = class_node_members(node_class)
    assert members is not None
    pins = {member.port: object() for member in members if member.form != "field"}
    return construct_class_node(node_class, field_values=field_values, pins=pins)


def test_a_receive_hands_its_native_half_what_it_carries_and_the_codec_its_choice_holds():
    by_default = a_node_built(MoqTrackReceive)
    assert (by_default._carries, by_default._codec) == ("named", None)
    h264 = a_node_built(MoqTrackReceive, content_type=PinTypeChoice(Annotated[EncodedVideo, EncodedCodecs("h264")]))
    assert (h264._carries, h264._codec) == ("encoded", "h264")


def test_the_names_a_gateway_and_a_track_give_are_cast_and_a_receive_reads_its_names_as_written():
    gateway = a_node_built(MoqGateway, kind="moq-relay", broadcast="Studio A")
    track = a_node_built(MoqTrack, gateway=MoqConnection("gateway"), track="Camera 1")
    assert gateway.settings_stated() == {"kind": "moq-relay", "broadcast": "studio-a"}
    assert track._track_name == "camera-1"
    receive = a_node_built(MoqTrackReceive, broadcast="Studio A.hang", track="Camera 1")
    assert (receive.broadcast, receive.track) == ("Studio A.hang", "Camera 1")


def test_a_record_and_relay_settings_travel_as_json_text():
    record = MoqRecord({"count": 3, "label": "tick"})
    assert MoqRecord.from_bytes(record.to_bytes()) == record
    settings = MoqRelaySettings({"token": "secret", "certificate_fingerprint": None})
    assert settings.to_bytes() == b'{"token":"secret","certificate_fingerprint":null}'
    assert MoqRelaySettings.from_bytes(settings.to_bytes()) == settings
    with pytest.raises(ValueError, match="one JSON object"):
        MoqRecord.from_bytes(b"[1]")


def test_relay_settings_refuse_a_key_no_gateway_takes_and_a_value_that_is_no_text():
    with pytest.raises(ValueError, match="`port` is no gateway setting"):
        MoqRelaySettings({"port": "4443"})
    with pytest.raises(ValueError, match="`token`"):
        MoqRelaySettings({"token": ""})


def test_a_track_refuses_a_catalog_track_name_and_a_catalog_entry_that_is_no_json_object():
    with pytest.raises(ValueError, match="writes its catalog on"):
        a_node_built(MoqTrack, track="Catalog.json")
    with pytest.raises(ValueError, match="no JSON text"):
        a_node_built(MoqTrack, track="camera", catalog=MoqCatalogEntry(section="video", entry="{"))
    with pytest.raises(ValueError, match="one JSON object"):
        a_node_built(MoqTrack, track="camera", catalog=MoqCatalogEntry(section="video", entry="[]"))


@pytest.mark.parametrize(
    ("node_class", "field_values", "refusal"),
    [
        (MoqTrack, {"track": "camera"}, "`gateway`"),
        (MoqTrack, {"gateway": MoqConnection("gateway")}, "`track`"),
        (MoqTrackReceive, {"gateway": MoqConnection("gateway"), "track": "camera"}, "`broadcast`"),
        (MoqTrackReceive, {"broadcast": "studio.hang", "track": "camera"}, "`gateway`"),
        (MoqTrackSend, {}, "`track`"),
    ],
)
def test_a_node_missing_what_it_needs_fails_at_setup_naming_it(
    node_class: type, field_values: "dict[str, Any]", refusal: str
):
    built = a_node_built(node_class, **field_values)

    class _Context:
        def open_module_host_table(self) -> None:
            raise AssertionError("refused before it reaches its native half")

    with pytest.raises(ValueError, match=refusal):
        built.setup(_Context())


def test_the_runtime_holds_a_gateway_to_its_relay_kinds_a_catalog_to_its_sections_and_max_age_above_zero():
    """The runtime refuses a value outside each of these, as it refuses one outside any menu or
    range a setting declares, before the node is built."""
    kind = the_port(MoqGateway, NODE_DECLARATION_INPUT_PORTS_STAMP, "kind")
    catalog = the_port(MoqTrack, NODE_DECLARATION_INPUT_PORTS_STAMP, "catalog")
    max_age = the_port(MoqTrackReceive, NODE_DECLARATION_INPUT_PORTS_STAMP, "max_age")
    assert kind["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "menu",
        "option_names": ["moq-relay", "cloudflare", "moqpro"],
    }
    [section] = [
        member
        for member in catalog["pin_declaration"]["pin_type"]["pin_type"]["members"]
        if member["name"] == "section"
    ]
    assert section["member_type"]["option_names"] == ["video", "audio", "text", "json", "binary"]
    assert max_age["pin_declaration"]["annotations"]["range"] == {"minimum": 0}
