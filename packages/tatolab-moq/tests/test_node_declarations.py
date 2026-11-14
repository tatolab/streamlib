# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The four nodes as a stream names them: their types, ports, references and
the config each refuses. Nothing here needs a runtime or a relay; the native
half's round trip through a relay is the crate's own test."""

from typing import Any

import pytest
from tatolab.moq import (
    MoqGateway,
    MoqGatewayConfig,
    MoqTrack,
    MoqTrackConfig,
    MoqTrackReceive,
    MoqTrackReceiveConfig,
    MoqTrackSend,
)
from tatolab.stream import (
    GetVariable,
    NodeReference,
    SetVariable,
    StreamBuilder,
    run_graph_generator,
    graph_generator,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

NODE_REFERENCE_SCHEMA_KEYWORD = "x-tatolab-node-reference"


@graph_generator
def a_track_sent_through_a_variable_and_read_back(stream_builder: StreamBuilder) -> None:
    gateway = stream_builder.add(
        MoqGateway,
        config={"kind": "moq-relay", "relay_url": "https://localhost:4443", "broadcast": "Studio A"},
    )
    camera_track = stream_builder.add(MoqTrack, config={"gateway": gateway, "track": "camera"})
    stream_builder.add(
        SetVariable, name="set-camera-track", config={"variable": "camera", "node_reference": camera_track}
    )
    get_camera_track = stream_builder.add(
        GetVariable, name="get-camera-track", config={"variable": "camera"}
    )
    stream_builder.add(MoqTrackSend, config={"track": get_camera_track})
    stream_builder.add(
        MoqTrackReceive,
        config={
            "gateway": gateway,
            "broadcast": "studio-a.hang",
            "track": "camera",
            "family": "packets",
            "codec": "h264",
        },
    )


def nodes_by_type(graph: "dict[str, Any]") -> "dict[str, dict[str, Any]]":
    return {node["type"]: node for node in graph["nodes"]}


def test_each_node_is_named_by_its_import_path_in_the_module_and_names_others_by_reference():
    nodes = nodes_by_type(run_graph_generator(a_track_sent_through_a_variable_and_read_back))
    gateway_name = nodes["tatolab.moq:MoqGateway"]["name"]

    assert nodes["tatolab.moq:MoqTrack"]["config"] == {
        "gateway": {"node": gateway_name},
        "track": "camera",
    }
    assert nodes["tatolab.moq:MoqTrackSend"]["config"] == {"track": {"node": "get-camera-track"}}
    assert nodes["tatolab.moq:MoqTrackReceive"]["config"]["gateway"] == {"node": gateway_name}


def node_reference_keyword_of(node_class: type, config_key: str) -> "dict[str, Any]":
    schema = getattr(node_class, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)
    [reference_branch] = [
        branch
        for branch in schema["properties"][config_key]["anyOf"]
        if NODE_REFERENCE_SCHEMA_KEYWORD in branch
    ]
    return reference_branch[NODE_REFERENCE_SCHEMA_KEYWORD]


def test_each_config_left_empty_takes_its_defaults_as_a_node_placed_with_no_config_does():
    assert MoqTrackConfig().track is None
    assert MoqTrackReceiveConfig().family == "records"
    for node_class in (MoqGateway, MoqTrack, MoqTrackSend, MoqTrackReceive):
        assert "required" not in getattr(node_class, NODE_DECLARATION_CONFIG_SCHEMA_STAMP)


def test_a_send_is_the_one_referrer_of_its_track_and_each_reference_names_its_node_type():
    assert node_reference_keyword_of(MoqTrackSend, "track") == {
        "node_type": "tatolab.moq:MoqTrack",
        "one_referrer_per_referenced_node": True,
    }
    assert node_reference_keyword_of(MoqTrack, "gateway") == {"node_type": "tatolab.moq:MoqGateway"}
    assert node_reference_keyword_of(MoqTrackReceive, "gateway") == {
        "node_type": "tatolab.moq:MoqGateway"
    }


def test_the_ports_are_a_gateways_records_settings_a_sends_content_and_a_receives_family_from_config():
    [settings] = getattr(MoqGateway, NODE_DECLARATION_INPUT_PORTS_STAMP)
    [content_in] = getattr(MoqTrackSend, NODE_DECLARATION_INPUT_PORTS_STAMP)
    [content_out] = getattr(MoqTrackReceive, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert settings["name"] == "settings"
    assert settings["family"]["family"] == "records"
    assert content_in["family"]["family"] == "packets"
    assert content_in["family"]["also_takes_families"] == ["records", "channels", "array"]
    assert content_out["family"]["family_from_config_key"] == "family"
    assert content_out["family"]["codec_from_config_key"] == "codec"
    assert getattr(MoqTrack, NODE_DECLARATION_INPUT_PORTS_STAMP) == []
    assert getattr(MoqTrack, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == []


def test_the_names_a_gateway_and_a_track_give_are_cast_and_a_receive_reads_its_names_as_written():
    gateway = MoqGatewayConfig(kind="moq-relay", broadcast="Studio A")
    track = MoqTrackConfig(gateway=NodeReference("gateway"), track="Camera 1")
    receive = MoqTrackReceiveConfig(
        gateway=NodeReference("gateway"),
        broadcast="Studio A.hang",
        track="Camera 1",
        family="records",
    )

    assert gateway.broadcast == "studio-a"
    assert gateway.settings_stated() == {"kind": "moq-relay", "broadcast": "studio-a"}
    assert track.track == "camera-1"
    assert (receive.broadcast, receive.track) == ("Studio A.hang", "Camera 1")


@pytest.mark.parametrize(
    ("config", "refusal"),
    [
        ({"family": "image"}, "`family`"),
        ({"family": "packets"}, "`codec`"),
        ({"family": "records", "codec": "h264"}, "`codec`"),
        ({"family": "records", "max_age": -1}, "`max_age`"),
        ({"family": "records", "max_age": True}, "`max_age`"),
    ],
)
def test_a_receive_refuses_a_family_or_budget_it_cannot_carry(config: "dict[str, Any]", refusal: str):
    with pytest.raises(ValueError, match=refusal):
        MoqTrackReceiveConfig(
            gateway=NodeReference("gateway"), broadcast="studio.hang", track="camera", **config
        )


def test_a_gateway_refuses_a_relay_kind_it_does_not_dial_and_a_track_a_catalog_of_no_section():
    with pytest.raises(ValueError, match="`kind`"):
        MoqGatewayConfig(kind="red5")  # pyright: ignore[reportArgumentType]
    with pytest.raises(ValueError, match="`catalog`"):
        MoqTrackConfig(
            gateway=NodeReference("gateway"), track="camera", catalog={"section": "data", "entry": {}}
        )
    with pytest.raises(ValueError, match="writes its catalog on"):
        MoqTrackConfig(gateway=NodeReference("gateway"), track="Catalog.json")
