# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The four nodes as a stream names them: their types, ports, connections and
the inputs each refuses. Nothing here needs a runtime or a relay; the native
half's round trip through a relay is the crate's own test."""

from typing import Annotated, Any

import pytest
from tatolab.moq import (
    MoqCatalogEntry,
    MoqGateway,
    MoqRecord,
    MoqRelaySettings,
    MoqTrack,
    MoqTrackReceive,
    MoqTrackSend,
    _catalog_entry_read_from,
    _gateway_settings_stated,
    _set_or_refused,
    _track_name_declared,
    _what_a_receive_carries,
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


def the_port(node_class: Any, ports_stamp: str, port_name: str) -> "dict[str, Any]":
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
    assert content_out["pin_declaration"]["pin_type"] == {
        "kind": "set_by_input",
        "input_name": "content_type",
    }


def test_each_node_is_one_function_whose_every_input_is_live():
    """Mental-revert: keep an input `SettingOnly()`, and a set of it rebuilds the node."""
    for node_function in (MoqGateway, MoqTrack, MoqTrackSend, MoqTrackReceive):
        assert not isinstance(node_function, type)
        for port in getattr(node_function, NODE_DECLARATION_INPUT_PORTS_STAMP):
            assert port.get("input_mode", {}).get("mode") != "setting_only", (node_function, port)


def test_a_receive_hands_its_native_half_what_it_carries_and_the_codec_its_choice_holds():
    assert _what_a_receive_carries(PinTypeChoice(MoqRecord)) == ("named", None)
    h264 = PinTypeChoice(Annotated[EncodedVideo, EncodedCodecs("h264")])
    assert _what_a_receive_carries(h264) == ("encoded", "h264")


def test_the_names_a_gateway_and_a_track_give_are_cast():
    assert _gateway_settings_stated({"kind": "moq-relay", "broadcast": "Studio A", "token": None}) == {
        "kind": "moq-relay",
        "broadcast": "studio-a",
    }
    assert _track_name_declared("Camera 1") == "camera-1"


def test_a_gateway_refuses_a_setting_that_holds_no_text():
    with pytest.raises(ValueError, match="`token`"):
        _gateway_settings_stated({"kind": "moq-relay", "token": ""})


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


def test_a_track_refuses_an_empty_or_catalog_track_name_and_a_catalog_entry_that_is_no_json_object():
    with pytest.raises(ValueError, match="`track` is not set"):
        _track_name_declared("")
    with pytest.raises(ValueError, match="writes its catalog on"):
        _track_name_declared("Catalog.json")
    with pytest.raises(ValueError, match="no JSON text"):
        _catalog_entry_read_from(MoqCatalogEntry(section="video", entry="{"))
    with pytest.raises(ValueError, match="one JSON object"):
        _catalog_entry_read_from(MoqCatalogEntry(section="video", entry="[]"))
    assert _catalog_entry_read_from(MoqCatalogEntry(section="video", entry='{"codec": "avc1"}')) == {
        "section": "video",
        "entry": {"codec": "avc1"},
    }


def test_a_connection_not_wired_is_refused_naming_its_input():
    with pytest.raises(ValueError, match="`gateway` is not set"):
        _set_or_refused("MoqTrack", "gateway", None, "a `MoqGateway`'s connection")


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
