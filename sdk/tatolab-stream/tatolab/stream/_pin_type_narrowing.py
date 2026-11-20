# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a pin's type narrows to, written beside it, and how an output's messages flow.

`Annotated[Texture, TexturePixelFormats("rgba8_unorm")]` names the formats a Texture holds, and
`Annotated[Asset, AssetMedia("image")]` the media an Asset must hold, and
`Annotated[EncodedVideo, EncodedCodecs("h264")]` the codecs encoded media is in. Each fills a
member of the pin type itself, its `pixel_formats`, its `media` or its `codecs`, so the runtime
judges a link and a picker offers assets by it; none is a key of the contract's `PinAnnotations`.

`FlowsAsValue()` on an output says its messages flow as a value: the runtime keeps its last
message and sends it to a reader linked after it was written.
"""

from __future__ import annotations

import typing
from typing import TYPE_CHECKING, Final, Literal, final

from ._contract_pin_types import BuiltInPinTypeName, ProjectAssetMedia, TexturePixelFormat

if TYPE_CHECKING:
    from ._contract_pin_types import PinType

__all__ = [
    "AssetMedia",
    "EncodedCodecs",
    "FlowsAsValue",
    "PinAnnotationNotTaken",
    "PinTypeNarrowing",
    "TexturePixelFormats",
    "an_output_flowing_as_a_value_by",
    "pin_type_narrowed_by",
]

_EVERY_TEXTURE_PIXEL_FORMAT: Final["tuple[str, ...]"] = typing.get_args(
    TexturePixelFormat.__value__
)
_EVERY_PROJECT_ASSET_MEDIA: Final["tuple[str, ...]"] = typing.get_args(
    ProjectAssetMedia.__value__
)


def _names_each_once(
    annotation: str, names: "tuple[str, ...]", every_name: "tuple[str, ...]", what: str
) -> "tuple[str, ...]":
    if not names:
        raise ValueError(f"{annotation}(...) takes at least one {what}, one of {every_name}.")
    for name in names:
        if name not in every_name:
            raise ValueError(
                f"{annotation}(...) takes each {what} as one of {every_name}, got {name!r}."
            )
    if len(set(names)) != len(names):
        raise ValueError(f"{annotation}(...) lists a {what} twice. To fix: list each once.")
    return names


class PinTypeNarrowing:
    """An annotation that fills a member of the pin type it is written beside."""

    __slots__ = ()

    #: Each pin type it narrows, as the contract tags it.
    narrows: typing.ClassVar["tuple[BuiltInPinTypeName, ...]"]
    #: The member of that pin type it fills.
    member: typing.ClassVar[str]

    def values(self) -> "list[str]":
        """What it fills the member with."""
        raise NotImplementedError


@final
class TexturePixelFormats(PinTypeNarrowing):
    """The formats a Texture holds: `Annotated[Texture, TexturePixelFormats("rgba8_unorm")]`.

    On an output, the formats it may send, each of which an input linked to it must take; on an
    input, the formats it takes.
    """

    __slots__ = ("_formats",)

    narrows = ("texture",)
    member = "pixel_formats"

    def __init__(self, pixel_format: TexturePixelFormat, /, *more: TexturePixelFormat) -> None:
        self._formats = _names_each_once(
            "TexturePixelFormats", (pixel_format, *more), _EVERY_TEXTURE_PIXEL_FORMAT, "format"
        )

    def values(self) -> "list[str]":
        return list(self._formats)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, TexturePixelFormats) and other._formats == self._formats

    def __hash__(self) -> int:
        return hash((TexturePixelFormats, self._formats))

    def __repr__(self) -> str:
        return f"TexturePixelFormats({', '.join(repr(each) for each in self._formats)})"


@final
class AssetMedia(PinTypeNarrowing):
    """The media an Asset holds one of: `Annotated[Asset, AssetMedia("image")]`.

    A picker offers only the project's assets that hold one of them.
    """

    __slots__ = ("_media",)

    narrows = ("asset",)
    member = "media"

    def __init__(self, media: ProjectAssetMedia, /, *more: ProjectAssetMedia) -> None:
        self._media = _names_each_once(
            "AssetMedia", (media, *more), _EVERY_PROJECT_ASSET_MEDIA, "media"
        )

    def values(self) -> "list[str]":
        return list(self._media)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, AssetMedia) and other._media == self._media

    def __hash__(self) -> int:
        return hash((AssetMedia, self._media))

    def __repr__(self) -> str:
        return f"AssetMedia({', '.join(repr(each) for each in self._media)})"


@final
class EncodedCodecs(PinTypeNarrowing):
    """The codecs encoded media is in: `Annotated[EncodedVideo, EncodedCodecs("h264")]`.

    On an output, the codecs it may send; one it names alone is the codec of every message it
    sends. On an input, the codecs it decodes, so a link from another codec is refused.
    Each is a codec string as a stream's facts spell it, such as `h264`, `h265` or `opus`.
    """

    __slots__ = ("_codecs",)

    narrows = ("encoded_video", "encoded_audio")
    member = "codecs"

    def __init__(self, codec: str, /, *more: str) -> None:
        codecs = (codec, *more)
        for each in codecs:
            if not isinstance(each, str) or not each:  # pyright: ignore[reportUnnecessaryIsInstance]
                raise ValueError(
                    f"EncodedCodecs(...) takes each codec as a non-empty string, got {each!r}."
                )
        if len(set(codecs)) != len(codecs):
            raise ValueError("EncodedCodecs(...) lists a codec twice. To fix: list each once.")
        self._codecs = codecs

    def values(self) -> "list[str]":
        return list(self._codecs)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, EncodedCodecs) and other._codecs == self._codecs

    def __hash__(self) -> int:
        return hash((EncodedCodecs, self._codecs))

    def __repr__(self) -> str:
        return f"EncodedCodecs({', '.join(repr(each) for each in self._codecs)})"


@final
class FlowsAsValue:
    """An output whose messages flow as a value: `OutputPin[Annotated[Texture, FlowsAsValue()]]`.

    The runtime keeps the last message it was sent and sends it to each reader linked after,
    so a node that sends one picture and then waits need not send it again. Its type must be
    one that may flow as a value.
    """

    __slots__ = ()

    def __eq__(self, other: object) -> bool:
        return isinstance(other, FlowsAsValue)

    def __hash__(self) -> int:
        return hash(FlowsAsValue)

    def __repr__(self) -> str:
        return "FlowsAsValue()"


type _FindingCode = Literal[
    "pin_annotation_not_taken", "pin_annotation_repeated", "pin_flow_not_taken"
]


class PinAnnotationNotTaken(Exception):
    """A narrowing or a flow written where it cannot apply, in the words its pin's refusal says."""

    def __init__(self, words: str, finding_code: _FindingCode) -> None:
        super().__init__(words)
        self.words = words
        self.finding_code: _FindingCode = finding_code


def _label_of(pin_type_name: str) -> str:
    from ._pin_value_stored_form import PIN_TYPE_DESCRIPTION_BY_NAME

    return PIN_TYPE_DESCRIPTION_BY_NAME[pin_type_name].label  # pyright: ignore[reportArgumentType]


def pin_type_narrowed_by(metadata: "list[object]", pin_type: PinType, pin_named: str) -> PinType:
    """`pin_type`, of the pin `pin_named` names, with each member a `PinTypeNarrowing` among
    `metadata` fills; a narrowing of another type, or two of one kind, are not taken."""
    narrowed: "dict[str, object]" = dict(pin_type)
    for entry in metadata:
        if not isinstance(entry, PinTypeNarrowing):
            continue
        if pin_type["type"] not in entry.narrows:
            raise PinAnnotationNotTaken(
                f"{pin_named} carries `{entry!r}`, which narrows the type "
                f"{' or '.join(_label_of(each) for each in entry.narrows)}, and the pin's type "
                f"is {_label_of(pin_type['type'])}. To fix: drop it.",
                "pin_annotation_not_taken",
            )
        if entry.member in narrowed:
            raise PinAnnotationNotTaken(
                f"{pin_named} carries two `{type(entry).__name__}`s; it takes one. To fix: list "
                f"everything in one.",
                "pin_annotation_repeated",
            )
        narrowed[entry.member] = entry.values()
    return typing.cast("PinType", narrowed)


def an_output_flowing_as_a_value_by(
    metadata: "list[object]",
    pin_type: PinType,
    pin_named: str,
    *,
    direction: Literal["input", "output"],
) -> bool:
    """Whether a `FlowsAsValue()` among `metadata` says the pin `pin_named` names, of
    `pin_type`, flows as a value; one on an input, or on a type that only streams, is not taken."""
    from ._pin_value_stored_form import PIN_TYPE_DESCRIPTION_BY_NAME

    stated = [entry for entry in metadata if isinstance(entry, FlowsAsValue)]
    if not stated:
        return False
    if len(stated) > 1:
        raise PinAnnotationNotTaken(
            f"{pin_named} carries {len(stated)} `FlowsAsValue()`s; it takes one. To fix: keep "
            f"one.",
            "pin_annotation_repeated",
        )
    if direction == "input":
        raise PinAnnotationNotTaken(
            f"{pin_named} carries `FlowsAsValue()`, which only an output states; an input's "
            f"form says how it reads. To fix: drop it, or declare the input as a field to read "
            f"its newest value.",
            "pin_flow_not_taken",
        )
    row = PIN_TYPE_DESCRIPTION_BY_NAME[pin_type["type"]]
    if "value" not in row.flows:
        raise PinAnnotationNotTaken(
            f"{pin_named} carries `FlowsAsValue()`, and its type {row.label} only streams. To "
            f"fix: drop it.",
            "pin_flow_not_taken",
        )
    return True
