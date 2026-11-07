# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Ports declared as class attributes by family, exercised without an engine."""

from typing import Any

import pytest

from tatolab.stream import (
    Array,
    AudioWindowContract,
    Channels,
    Image,
    Input,
    Output,
    Packets,
    Records,
    from_config,
    node,
)


def _the_port_named(ports: "list[dict[str, Any]]", port_name: str) -> "dict[str, Any]":
    return next(port for port in ports if port["name"] == port_name)


def test_an_image_port_pair_declares_its_family_and_formats_under_the_attribute_name():
    @node
    class Tonemap:
        video_in = Input(Image, delivery="newest", formats=("rgba16_float", "rgba8_unorm"))
        video_out = Output(Image, formats=("rgba8_unorm",), description="tonemapped")

    (declared_input,) = Tonemap.__tatolab_node_input_ports__
    (declared_output,) = Tonemap.__tatolab_node_output_ports__
    assert declared_input == {
        "name": "video_in",
        "description": "",
        "delivery_profile": "newest",
        "family": {"family": "image", "formats": ["rgba16_float", "rgba8_unorm"]},
    }
    assert declared_output == {
        "name": "video_out",
        "description": "tonemapped",
        "family": {"family": "image", "formats": ["rgba8_unorm"]},
    }


def test_an_array_output_declares_its_names_and_residency():
    @node(execution="manual")
    class Detector:
        boxes = Output(Array, names=("boxes", "scores"), residency="gpu")

    (declared_output,) = Detector.__tatolab_node_output_ports__
    assert declared_output["family"] == {
        "family": "array",
        "names": {"stated": ["boxes", "scores"]},
        "residency": "gpu",
    }


def test_a_channels_output_reads_its_names_and_rate_from_config():
    @node(execution="manual")
    class EngineTelemetry:
        signals = Output(
            Channels,
            names=from_config("channel_names"),
            rate=from_config("sample_rate"),
            dtype="f64",
        )

    (declared_output,) = EngineTelemetry.__tatolab_node_output_ports__
    assert declared_output["family"] == {
        "family": "channels",
        "names": {"from_config": {"config_key": "channel_names"}},
        "rate": {"from_config": {"config_key": "sample_rate"}},
        "dtype": "f64",
    }


def test_a_channels_input_requires_names_and_takes_a_window():
    @node
    class Meter:
        audio = Input(
            Channels,
            delivery="ordered",
            requires=("left", "right"),
            window=AudioWindowContract(
                sample_rate=48_000, dtype="f32", window_size=960, hop=960
            ),
        )

    (declared_input,) = Meter.__tatolab_node_input_ports__
    assert declared_input["family"] == {"family": "channels", "requires": ["left", "right"]}
    assert declared_input["audio_window"]["window_size"] == 960


def test_a_packets_pair_declares_its_codec_and_the_codecs_an_input_decodes():
    @node
    class Repackager:
        packets_in = Input(Packets, delivery="ordered", requires_codec=("h264", "h265"))
        packets_out = Output(Packets, codec="h264")

    assert _the_port_named(Repackager.__tatolab_node_input_ports__, "packets_in")["family"] == {
        "family": "packets",
        "requires_codec": ["h264", "h265"],
    }
    assert _the_port_named(Repackager.__tatolab_node_output_ports__, "packets_out")[
        "family"
    ] == {"family": "packets", "codec": "h264"}


def test_a_records_pair_declares_its_keys_and_the_keys_an_input_requires():
    @node
    class Router:
        events_in = Input(Records, delivery="ordered", requires=("kind",), borrow_quota=4)
        events_out = Output(Records, names=("kind", "at"))

    declared_input = _the_port_named(Router.__tatolab_node_input_ports__, "events_in")
    assert declared_input["family"] == {"family": "records", "requires": ["kind"]}
    assert declared_input["borrow_quota"] == 4
    assert _the_port_named(Router.__tatolab_node_output_ports__, "events_out")["family"] == {
        "family": "records",
        "names": {"stated": ["kind", "at"]},
    }


def test_a_subclass_inherits_the_ports_its_base_declares():
    class BaseFilter:
        video_in = Input(Image, delivery="newest")

    @node
    class Repackager(BaseFilter):
        packets_out = Output(Packets)

    assert [port["name"] for port in Repackager.__tatolab_node_input_ports__] == ["video_in"]
    (declared_output,) = Repackager.__tatolab_node_output_ports__
    assert declared_output["family"] == {"family": "packets"}


def test_a_mistyped_key_is_refused_at_import():
    with pytest.raises(TypeError, match="requirs"):
        Input(Channels, delivery="ordered", requirs=("left",))  # pyright: ignore[reportCallIssue]


@pytest.mark.parametrize(
    "declare_the_class",
    [
        lambda: type("ImageWithNames", (), {"video": Output(Image, names=("x",))}),
        lambda: type("ArrayWithCodec", (), {"tensor": Output(Array, codec="h264")}),
        lambda: type(
            "ImageRequiringNames", (), {"video": Input(Image, delivery="newest", requires=("x",))}
        ),
        lambda: type(
            "RecordsWithFormats", (), {"events": Input(Records, delivery="ordered", formats=("rgba8_unorm",))}
        ),
    ],
)
def test_a_key_the_family_does_not_take_is_refused_naming_the_port_and_the_key(declare_the_class):
    node_class = declare_the_class()
    port_name = next(name for name in vars(node_class) if not name.startswith("__"))
    with pytest.raises(ValueError, match=rf"port '{port_name}' of family \w+ states `\w+=`"):
        node(execution="manual")(node_class)


def test_an_input_without_a_delivery_is_refused():
    with pytest.raises(ValueError, match="delivery="):
        Input(Image)


def test_a_family_that_is_not_one_of_the_five_is_refused():
    with pytest.raises(TypeError, match="Image, Array, Channels, Packets, Records"):
        Output("image")  # pyright: ignore[reportArgumentType]


def test_names_given_as_a_bare_string_are_refused():
    with pytest.raises(TypeError, match="tuple of strings"):
        Output(Records, names="kind")  # pyright: ignore[reportArgumentType]


def test_an_image_format_that_is_no_engine_texture_name_is_refused():
    with pytest.raises(ValueError, match="rgba8_unorm"):
        Output(Image, formats=("rgba8",))
