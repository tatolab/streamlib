# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin's type is narrowed by the annotations written beside it, and an output states that it flows as a value.

`TexturePixelFormats` fills a Texture's `pixel_formats` and `AssetMedia` an Asset's `media`, on a
class node's pin or field and on the type a `PinTypeChoice` chooses. `FlowsAsValue()` makes an output's flow Value, so the runtime keeps its last message
for a reader linked late. Each is refused, by name, where it cannot apply.
"""

from __future__ import annotations

from typing import Annotated, Any

import pytest

from tatolab.stream import (
    Asset,
    AssetMedia,
    Audio,
    FlowsAsValue,
    NodeDeclarationRefusal,
    PinTypeChoice,
    Texture,
    TexturePixelFormats,
    node,
)
from tatolab.stream._class_node_declaration import read_class_node_ports
from tatolab.stream._class_node_pins import InputPin, OutputPin


def test_a_class_nodes_pins_and_fields_carry_the_formats_media_and_flow_written_beside_them():
    @node
    class Picture:
        asset: Annotated[Asset, AssetMedia("image", "video")] | None = None
        video: InputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm", "nv12")]]
        picture: OutputPin[
            Annotated[Texture, TexturePixelFormats("bgra8_unorm"), FlowsAsValue()]
        ]
        streamed: OutputPin[Annotated[Texture, TexturePixelFormats("bgra8_unorm")]]

        def process(self, ctx: Any) -> None:
            pass

    inputs, outputs = read_class_node_ports(Picture)
    declared = {port["name"]: port["pin_declaration"] for port in (*inputs, *outputs)}
    assert declared["asset"]["pin_type"]["pin_type"] == {"type": "asset", "media": ["image", "video"]}
    assert declared["video"] == {
        "pin_type": {
            "kind": "is",
            "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm", "nv12"]},
        },
        "flow": "stream",
    }
    assert declared["picture"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "texture", "pixel_formats": ["bgra8_unorm"]}},
        "flow": "value",
    }
    assert declared["streamed"]["flow"] == "stream"


def test_the_type_a_pin_type_choice_chooses_takes_the_same_narrowing():
    assert PinTypeChoice(Texture, TexturePixelFormats("nv12")).chosen == {
        "type": "texture",
        "pixel_formats": ["nv12"],
    }
    assert PinTypeChoice(Asset, AssetMedia("lut")).chosen == {"type": "asset", "media": ["lut"]}


@pytest.mark.parametrize(
    ("annotation", "refused_because"),
    [
        ("OutputPin[Annotated[Audio, TexturePixelFormats('nv12')]]", "narrows the type Texture"),
        ("OutputPin[Annotated[Audio, FlowsAsValue()]]", "its type Audio only streams"),
        ("InputPin[Annotated[Texture, FlowsAsValue()]]", "only an output states"),
        (
            "OutputPin[Annotated[Texture, TexturePixelFormats('nv12'), TexturePixelFormats('nv12')]]",
            "two `TexturePixelFormats`s",
        ),
    ],
)
def test_a_narrowing_or_a_flow_where_it_cannot_apply_is_refused_by_name(
    annotation: str, refused_because: str
):
    namespace: "dict[str, Any]" = {
        "Annotated": Annotated,
        "Audio": Audio,
        "FlowsAsValue": FlowsAsValue,
        "InputPin": InputPin,
        "OutputPin": OutputPin,
        "Texture": Texture,
        "TexturePixelFormats": TexturePixelFormats,
    }
    with pytest.raises(NodeDeclarationRefusal, match=refused_because):

        @node
        class Refused:
            __annotations__ = {"pin": eval(annotation, namespace)}  # noqa: S307

            def process(self, ctx: Any) -> None:
                pass


def test_a_narrowing_names_each_of_its_values_once_from_what_the_runtime_reads():
    with pytest.raises(ValueError, match="takes each format as one of"):
        TexturePixelFormats("rgb8")  # pyright: ignore[reportArgumentType]
    with pytest.raises(ValueError, match="lists a media twice"):
        AssetMedia("image", "image")
    assert TexturePixelFormats("nv12") == TexturePixelFormats("nv12")
    assert repr(AssetMedia("image", "lut")) == "AssetMedia('image', 'lut')"
