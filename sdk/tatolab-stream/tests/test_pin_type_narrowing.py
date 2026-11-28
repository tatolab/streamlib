# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A pin's type is narrowed by the annotations written beside it, and an output states that it flows as a value.

`TexturePixelFormats` fills a Texture's `pixel_formats`, `AssetMedia` an Asset's `media` and
`EncodedCodecs` encoded media's `codecs`, on a
class node's pin or field and on the type a `PinTypeChoice` chooses. `FlowsAsValue()` makes an output's flow Value, so the runtime keeps its last message
for a reader linked late. Each is refused, by name, where it cannot apply.
"""

from __future__ import annotations

import inspect
from typing import Annotated, Any, Callable

import pytest

from tatolab.stream import (
    Asset,
    AssetMedia,
    Audio,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    FlowsAsValue,
    Input,
    NodeDeclarationRefusal,
    PinTypeChoice,
    Texture,
    TexturePixelFormats,
    node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._class_node_pins import InputPin, OutputPin


def test_a_node_functions_pins_and_inputs_carry_the_formats_media_and_flow_written_beside_them():
    @node(execution="reactive")
    def Picture(
        *,
        asset: Input[Annotated[Asset, AssetMedia("image", "video")] | None] = Input(None),
        video: InputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm", "nv12")]],
        picture: OutputPin[
            Annotated[Texture, TexturePixelFormats("bgra8_unorm"), FlowsAsValue()]
        ],
        streamed: OutputPin[Annotated[Texture, TexturePixelFormats("bgra8_unorm")]],
    ) -> None:
        return None

    inputs = getattr(Picture, NODE_DECLARATION_INPUT_PORTS_STAMP)
    outputs = getattr(Picture, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
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


def test_encoded_media_carries_its_codecs_and_an_output_of_one_codec_sends_it_on_every_packet():
    @node(execution="reactive")
    def Transcode(
        *,
        encoded: InputPin[Annotated[EncodedVideo, EncodedCodecs("h264", "h265")]],
        reencoded: OutputPin[Annotated[EncodedVideo, EncodedCodecs("h265")]],
        voice: OutputPin[Annotated[EncodedAudio, EncodedCodecs("opus")]],
        either: OutputPin[Annotated[EncodedAudio, EncodedCodecs("opus", "aac")]],
    ) -> None:
        return None

    inputs = getattr(Transcode, NODE_DECLARATION_INPUT_PORTS_STAMP)
    outputs = getattr(Transcode, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    ports = {port["name"]: port for port in (*inputs, *outputs)}
    assert ports["encoded"]["pin_declaration"]["pin_type"]["pin_type"] == {
        "type": "encoded_video",
        "codecs": ["h264", "h265"],
    }


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
        ("OutputPin[Annotated[Texture, EncodedCodecs('h264')]]", "narrows the type Encoded"),
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
        "EncodedCodecs": EncodedCodecs,
        "FlowsAsValue": FlowsAsValue,
        "InputPin": InputPin,
        "OutputPin": OutputPin,
        "Texture": Texture,
        "TexturePixelFormats": TexturePixelFormats,
    }
    def Refused(**_: Any) -> None: ...

    pin = eval(annotation, namespace)  # noqa: S307
    Refused.__signature__ = inspect.Signature(  # pyright: ignore[reportFunctionMemberAccess]
        [inspect.Parameter("pin", inspect.Parameter.KEYWORD_ONLY, annotation=pin)]
    )
    Refused.__annotations__ = {"pin": pin}
    with pytest.raises(NodeDeclarationRefusal, match=refused_because):
        node(execution="reactive")(Refused)


def test_a_narrowing_names_each_of_its_values_once_from_what_the_runtime_reads():
    with pytest.raises(ValueError, match="takes each format as one of"):
        TexturePixelFormats("rgb8")  # pyright: ignore[reportArgumentType]
    with pytest.raises(ValueError, match="lists a media twice"):
        AssetMedia("image", "image")
    assert TexturePixelFormats("nv12") == TexturePixelFormats("nv12")
    assert repr(AssetMedia("image", "lut")) == "AssetMedia('image', 'lut')"
    with pytest.raises(ValueError, match="lists a codec twice"):
        EncodedCodecs("h264", "h264")
    with pytest.raises(ValueError, match="non-empty string"):
        EncodedCodecs("")
    assert repr(EncodedCodecs("h264", "h265")) == "EncodedCodecs('h264', 'h265')"
