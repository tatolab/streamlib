# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Ports declared as class attributes by family: `video = Output(Image)`.

The attribute's name is the port's name. `@node.process` collects every `Input` and
`Output` on the class and its bases and stamps them on the class; `connect`
compares the two ends' families, names, codec and formats before a frame flows.
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING, Any, Final, Generic, Optional, TypeVar, Union, overload

if TYPE_CHECKING:
    from ._node_context_protocols import ChannelsBlock, PacketsUnit
    from .array_frame import ArrayFrame
    from .image_frame import ImageFrame
    from ._node_declaration import AudioWindowContract
    from .stream_facts import ImageStreamFrameRate

_PortFamilyReadType = TypeVar("_PortFamilyReadType", covariant=True)

__all__ = [
    "Array",
    "Channels",
    "Image",
    "Input",
    "Output",
    "Packets",
    "PortFamily",
    "PortNamesFromConfig",
    "Records",
    "from_config",
]


class PortFamily(Generic[_PortFamilyReadType]):
    """One of the five memory layouts a port carries; a link joins two ports of one family.

    Its type parameter is what a read through a port of the family returns.
    """

    __slots__ = ("_declaration_name", "_wire_name")

    def __init__(self, declaration_name: str) -> None:
        self._declaration_name = declaration_name
        self._wire_name = declaration_name.lower()

    @property
    def declaration_name(self) -> str:
        """The name a declaration spells this family by, such as `Image`."""
        return self._declaration_name

    def __repr__(self) -> str:
        return self._declaration_name


Image: Final[PortFamily[ImageFrame]] = PortFamily("Image")
"""A GPU surface with its size, pixel format and colour."""
Array: Final[PortFamily[ArrayFrame]] = PortFamily("Array")
"""Named tensors, on the GPU or the CPU."""
Channels: Final[PortFamily[ChannelsBlock]] = PortFamily("Channels")
"""Named sample channels at one rate."""
Packets: Final[PortFamily[PacketsUnit]] = PortFamily("Packets")
"""Encoded units of one codec."""
Records: Final[PortFamily["dict[str, Any]"]] = PortFamily("Records")
"""A dict per message, keyed by the declared names."""

_PORT_FAMILIES: Final = (Image, Array, Channels, Packets, Records)
_FAMILIES_WITH_NAMES: Final = (Array, Channels, Records)
_ARRAY_RESIDENCIES: Final = ("gpu", "cpu")
_IMAGE_TEXTURE_FORMATS: Final = (
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
    "nv12",
)
"""The engine's texture format wire names, the spellings `formats=` takes."""
_CHANNELS_SAMPLE_TYPES: Final = ("f32", "f64")
_DELIVERY_PROFILES: Final = ("newest", "ordered")


class PortNamesFromConfig:
    """A value an output reads from one key of its node's config when `connect` resolves it."""

    __slots__ = ("_config_key",)

    def __init__(self, config_key: str) -> None:
        if not isinstance(config_key, str) or not config_key:
            raise TypeError(
                f"from_config takes the name of a config key as a non-empty str, got "
                f"{config_key!r}"
            )
        self._config_key = config_key

    @property
    def config_key(self) -> str:
        """The config key the value is read from."""
        return self._config_key

    def _as_declaration(self) -> "dict[str, Any]":
        return {"from_config": {"config_key": self._config_key}}

    def __repr__(self) -> str:
        return f"from_config({self._config_key!r})"


def from_config(config_key: str) -> PortNamesFromConfig:
    """Read an output's names, family or codec, or a Channels output's rate, from `config_key` of its node's config."""
    return PortNamesFromConfig(config_key)


def _refuse_a_port_family_that_is_not_one(
    family: "PortFamily[_PortFamilyReadType]", declared_as: str
) -> "PortFamily[_PortFamilyReadType]":
    if not isinstance(family, PortFamily):  # pyright: ignore[reportUnnecessaryIsInstance]
        raise TypeError(
            f"{declared_as}({family!r}) — a port's family is one of tatolab.stream's "
            f"{', '.join(repr(each) for each in _PORT_FAMILIES)}"
        )
    return family


def _image_texture_formats(value: object) -> "tuple[str, ...]":
    formats = _tuple_of_strings(value, "formats")
    for texture_format in formats:
        if texture_format not in _IMAGE_TEXTURE_FORMATS:
            raise ValueError(
                f"`formats=` takes the engine's texture format names "
                f"({', '.join(_IMAGE_TEXTURE_FORMATS)}), got {texture_format!r}"
            )
    return formats


def _tuple_of_strings(value: object, key: str) -> "tuple[str, ...]":
    if isinstance(value, str) or not isinstance(value, (tuple, list)):
        raise TypeError(f"`{key}=` takes a tuple of strings, got {value!r}")
    for entry in value:
        if not isinstance(entry, str):
            raise TypeError(f"`{key}=` takes a tuple of strings, and holds {entry!r}")
    return tuple(value)


class _DeclaredPortAttribute(Generic[_PortFamilyReadType]):
    __slots__ = ("_port_name", "family", "description")

    def __init__(self, family: "PortFamily[_PortFamilyReadType]", description: str) -> None:
        self._port_name: Optional[str] = None
        self.family = family
        if not isinstance(description, str):
            raise TypeError(f"`description=` takes a str, got {description!r}")
        self.description = description

    def __set_name__(self, owner: type, attribute_name: str) -> None:
        self._port_name = attribute_name

    @property
    def port_name(self) -> str:
        """The port's name: the class attribute it is assigned to."""
        if self._port_name is None:
            raise ValueError(
                f"{self!r} is not assigned to a class attribute, which is what names a port"
            )
        return self._port_name

    def _refuse_keys_the_family_does_not_take(
        self, direction: str, stated_keys: "dict[str, bool]"
    ) -> None:
        for key, stated_but_not_taken in stated_keys.items():
            if stated_but_not_taken:
                raise ValueError(
                    f"{direction} port {self.port_name!r} of family {self.family!r} states "
                    f"`{key}=`, which a {self.family!r} {direction} does not take"
                )


class Input(_DeclaredPortAttribute[_PortFamilyReadType]):
    """An input port of one family, declared as a class attribute: `audio = Input(Channels, delivery="ordered")`.

    `delivery=` is required: `"newest"` drains to the most recent message,
    `"ordered"` receives them in publication order. `borrow_quota=` is how many
    read messages each link into the port may hold beyond its mailbox at once.
    `requires=` names the output's names this input needs (Array, Channels,
    Records), `requires_codec=` the codecs it decodes (Packets), `formats=` the
    pixel formats it reads (Image), and `window=` the audio window it wants
    (Channels). `also_takes=` names the families the input takes a link from
    besides its own; a read of such an input hands back the arriving family's
    type. `connect` refuses a link whose output does not meet them.
    `default=` is the number a Channels input reads while nothing is linked
    into it and no pin value is set on its node.
    """

    __slots__ = (
        "delivery",
        "borrow_quota",
        "requires",
        "requires_codec",
        "formats",
        "window",
        "also_takes",
        "default",
    )

    @overload
    def __init__(
        self,
        family: PortFamily[_PortFamilyReadType],
        *,
        delivery: Optional[str] = None,
        borrow_quota: Optional[int] = None,
        requires: tuple[str, ...] = (),
        requires_codec: tuple[str, ...] = (),
        formats: tuple[str, ...] = (),
        window: Optional[AudioWindowContract] = None,
        also_takes: tuple[()] = (),
        default: Optional[float] = None,
        description: str = "",
    ) -> None: ...

    @overload
    def __init__(
        self: Input[Any],
        family: PortFamily[Any],
        *,
        delivery: Optional[str] = None,
        borrow_quota: Optional[int] = None,
        requires: tuple[str, ...] = (),
        requires_codec: tuple[str, ...] = (),
        formats: tuple[str, ...] = (),
        window: Optional[AudioWindowContract] = None,
        also_takes: tuple[PortFamily[Any], ...],
        default: Optional[float] = None,
        description: str = "",
    ) -> None: ...

    def __init__(
        self,
        family: PortFamily[Any],
        *,
        delivery: Optional[str] = None,
        borrow_quota: Optional[int] = None,
        requires: tuple[str, ...] = (),
        requires_codec: tuple[str, ...] = (),
        formats: tuple[str, ...] = (),
        window: Optional[AudioWindowContract] = None,
        also_takes: tuple[PortFamily[Any], ...] = (),
        default: Optional[float] = None,
        description: str = "",
    ) -> None:
        super().__init__(_refuse_a_port_family_that_is_not_one(family, "Input"), description)
        if isinstance(also_takes, PortFamily) or not isinstance(also_takes, (tuple, list)):
            raise TypeError(f"`also_takes=` takes a tuple of port families, got {also_takes!r}")
        self.also_takes: tuple[PortFamily[Any], ...] = tuple(
            _refuse_a_port_family_that_is_not_one(each, "Input(also_takes=…)")
            for each in also_takes
        )
        if delivery not in _DELIVERY_PROFILES:
            raise ValueError(
                f"Input({family!r}) must declare `delivery=`, one of "
                f"{', '.join(repr(each) for each in _DELIVERY_PROFILES)}; got {delivery!r}"
            )
        if borrow_quota is not None and (
            isinstance(borrow_quota, bool)
            or not isinstance(borrow_quota, int)
            or borrow_quota < 0
        ):
            raise ValueError(
                f"invalid borrow_quota {borrow_quota!r}: must be a whole number of messages, "
                f"0 or more"
            )
        self.delivery: str = delivery
        self.borrow_quota = borrow_quota
        self.requires = _tuple_of_strings(requires, "requires")
        self.requires_codec = _tuple_of_strings(requires_codec, "requires_codec")
        self.formats = _image_texture_formats(formats)
        self.window = window
        if default is not None and (
            isinstance(default, bool) or not isinstance(default, (int, float))
        ):
            raise TypeError(f"`default=` takes a number, got {default!r}")
        self.default = default

    def _as_port_marker(self) -> "dict[str, Any]":
        from ._node_declaration import _audio_window_declaration

        families_the_input_takes = (self.family, *self.also_takes)
        self._refuse_keys_the_family_does_not_take(
            "input",
            {
                "requires": bool(self.requires)
                and not any(each in _FAMILIES_WITH_NAMES for each in families_the_input_takes),
                "requires_codec": bool(self.requires_codec)
                and Packets not in families_the_input_takes,
                "formats": bool(self.formats) and self.family is not Image,
                "window": self.window is not None and self.family is not Channels,
                "default": self.default is not None and self.family is not Channels,
            },
        )
        family_declaration: "dict[str, Any]" = {"family": self.family._wire_name}
        if self.requires:
            family_declaration["requires"] = list(self.requires)
        if self.requires_codec:
            family_declaration["requires_codec"] = list(self.requires_codec)
        if self.formats:
            family_declaration["formats"] = list(self.formats)
        if self.also_takes:
            family_declaration["also_takes_families"] = [
                each._wire_name for each in self.also_takes
            ]
        marker: "dict[str, Any]" = {
            "name": self.port_name,
            "description": self.description,
            "delivery_profile": self.delivery,
            "family": family_declaration,
        }
        if self.borrow_quota is not None:
            marker["borrow_quota"] = self.borrow_quota
        if self.default is not None:
            marker["pin_default"] = float(self.default)
        if self.window is not None:
            marker["audio_window"] = _audio_window_declaration(
                self.window, self.port_name, self.delivery
            )
        return marker

    def __repr__(self) -> str:
        return f"Input({self.family!r}, delivery={self.delivery!r})"


class Output(_DeclaredPortAttribute[_PortFamilyReadType]):
    """An output port of one family, declared as a class attribute: `video = Output(Image)`.

    `names=` states the output's names (Array, Channels, Records), as a tuple of
    strings or `from_config("key")`. `codec=` is a Packets output's codec,
    `rate=` and `dtype=` a Channels output's sample rate and type, `rate=` an
    Image output's frame rate (an int, an `ImageStreamFrameRate`, or
    `from_config("key")`), `formats=` an Image output's pixel formats and
    `residency=` where an Array output's tensors live. `family_from=
    from_config("key")` reads the family from the node's config when
    `connect` resolves it, the positional family standing while the key is
    unset; `codec=from_config("key")` reads a codec the same way.
    """

    __slots__ = (
        "names",
        "codec",
        "rate",
        "dtype",
        "formats",
        "residency",
        "family_from",
    )

    def __init__(
        self,
        family: "PortFamily[_PortFamilyReadType]",
        *,
        names: "Union[tuple[str, ...], PortNamesFromConfig, None]" = None,
        codec: "Union[str, PortNamesFromConfig, None]" = None,
        rate: "Union[int, ImageStreamFrameRate, PortNamesFromConfig, None]" = None,
        dtype: Optional[str] = None,
        formats: "tuple[str, ...]" = (),
        residency: Optional[str] = None,
        family_from: "Optional[PortNamesFromConfig]" = None,
        description: str = "",
    ) -> None:
        super().__init__(_refuse_a_port_family_that_is_not_one(family, "Output"), description)
        if family_from is not None and not isinstance(family_from, PortNamesFromConfig):
            raise TypeError(f"`family_from=` takes from_config(...), got {family_from!r}")
        self.family_from = family_from
        if names is not None and not isinstance(names, PortNamesFromConfig):
            names = _tuple_of_strings(names, "names")
        self.names = names
        if codec is not None and not isinstance(codec, PortNamesFromConfig) and (
            not isinstance(codec, str) or not codec
        ):
            raise TypeError(f"`codec=` takes a non-empty str or from_config(...), got {codec!r}")
        self.codec = codec
        from .stream_facts import ImageStreamFrameRate

        if isinstance(rate, ImageStreamFrameRate):
            if self.family is not Image:
                raise ValueError(
                    f"`rate=` takes an ImageStreamFrameRate only on an Image output, got {rate!r}"
                )
            if rate.numerator <= 0 or rate.denominator <= 0:
                raise ValueError(f"`rate=` takes a positive frame rate, got {rate!r}")
        elif rate is not None and not isinstance(rate, PortNamesFromConfig) and (
            isinstance(rate, bool) or not isinstance(rate, int) or rate <= 0
        ):
            raise ValueError(
                f"`rate=` takes a positive int, an ImageStreamFrameRate or from_config(...), "
                f"got {rate!r}"
            )
        self.rate = rate
        if dtype is not None and dtype not in _CHANNELS_SAMPLE_TYPES:
            raise ValueError(
                f"`dtype=` is one of {', '.join(repr(each) for each in _CHANNELS_SAMPLE_TYPES)}, "
                f"got {dtype!r}"
            )
        self.dtype = dtype
        self.formats = _image_texture_formats(formats)
        if residency is not None and residency not in _ARRAY_RESIDENCIES:
            raise ValueError(
                f"`residency=` is one of {', '.join(repr(each) for each in _ARRAY_RESIDENCIES)}, "
                f"got {residency!r}"
            )
        self.residency = residency

    def _as_port_marker(self) -> "dict[str, Any]":
        from .stream_facts import ImageStreamFrameRate

        self._refuse_keys_the_family_does_not_take(
            "output",
            {
                "names": self.names is not None
                and self.family not in _FAMILIES_WITH_NAMES
                and self.family_from is None,
                "codec": self.codec is not None
                and self.family is not Packets
                and not (isinstance(self.codec, PortNamesFromConfig) and self.family_from is not None),
                "rate": self.rate is not None and self.family not in (Channels, Image),
                "dtype": self.dtype is not None and self.family is not Channels,
                "formats": bool(self.formats) and self.family is not Image,
                "residency": self.residency is not None and self.family is not Array,
            },
        )
        family_declaration: "dict[str, Any]" = {"family": self.family._wire_name}
        if isinstance(self.names, PortNamesFromConfig):
            family_declaration["names"] = self.names._as_declaration()
        elif self.names is not None:
            family_declaration["names"] = {"stated": list(self.names)}
        if isinstance(self.codec, PortNamesFromConfig):
            family_declaration["codec_from_config_key"] = self.codec.config_key
        elif self.codec is not None:
            family_declaration["codec"] = self.codec
        if self.family_from is not None:
            family_declaration["family_from_config_key"] = self.family_from.config_key
        rate_key = "frame_rate" if self.family is Image else "rate"
        if isinstance(self.rate, PortNamesFromConfig):
            family_declaration[rate_key] = self.rate._as_declaration()
        elif isinstance(self.rate, ImageStreamFrameRate):
            family_declaration[rate_key] = {"stated": dataclasses.asdict(self.rate)}
        elif self.rate is not None and self.family is Image:
            family_declaration[rate_key] = {"stated": {"numerator": self.rate, "denominator": 1}}
        elif self.rate is not None:
            family_declaration[rate_key] = {"stated": self.rate}
        if self.dtype is not None:
            family_declaration["dtype"] = self.dtype
        if self.formats:
            family_declaration["formats"] = list(self.formats)
        if self.residency is not None:
            family_declaration["residency"] = self.residency
        marker: "dict[str, Any]" = {
            "name": self.port_name,
            "description": self.description,
            "family": family_declaration,
        }
        return marker

    def __repr__(self) -> str:
        return f"Output({self.family!r})"
