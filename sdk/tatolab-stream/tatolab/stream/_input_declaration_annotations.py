# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a class node's input takes its value and its messages, each written as one object on its annotation.

`radius: Annotated[float, SettingOnly()] = 4.0` declares a field that is read at setup, from its
set value or from a Value wire, each change of either assigned to the field live. `WireOnly()` declares a field that takes only a wire, and `InputValueLifetime("reset")`
one whose live value is forgotten at Stop. `voice: InputPin[Annotated[Audio,
StreamDelivery("ordered")]]` declares how a Stream input reads its messages. None of them is a
pin annotation of the contract: each says how the engine serves the input, so each is sent as a
key of the port beside its pin declaration.
"""

from __future__ import annotations

from typing import Final, Literal, Optional, final, get_args

__all__ = [
    "INPUT_DECLARATION_ANNOTATION_CLASSES",
    "InputValueLifetime",
    "PublishedOnAnOutput",
    "SettingOnly",
    "SettingReadTime",
    "StreamDelivery",
    "WireOnly",
]

type InputValueLifetimeName = Literal["kept", "reset"]
"""Each lifetime an input's value may have, as the engine names it."""

type StreamDeliveryProfile = Literal["ordered", "newest"]
"""Each way a Stream input may read its messages, as the engine names it."""


class _InputModeAnnotation:
    """An annotation that states an input's mode and takes no argument."""

    __slots__ = ()

    def __eq__(self, other: object) -> bool:
        return type(other) is type(self)

    def __hash__(self) -> int:
        return hash(type(self))

    def __repr__(self) -> str:
        return f"{type(self).__name__}()"


type SettingReadTime = Literal["at_setup", "each_run"]
"""When a setting is read, as the engine names it."""


@final
class SettingOnly:
    """A field the node reads at setup: `Annotated[int, SettingOnly()]`.

    It takes a set value or a wire from an output that flows as a value, which the engine reads
    in the node's place, handing the node each new value it carries; a change is assigned to the
    field live and rebuilds nothing. `SettingOnly(read="each_run")` is read on every run, which
    only the runtime's own nodes declare."""

    __slots__ = ("_read",)

    def __init__(self, *, read: SettingReadTime = "at_setup") -> None:
        read_times = get_args(SettingReadTime.__value__)
        if read not in read_times:
            raise ValueError(
                f"SettingOnly(read=...) takes one of {', '.join(repr(each) for each in read_times)}, "
                f"got {read!r}."
            )
        self._read: SettingReadTime = read

    @property
    def read(self) -> SettingReadTime:
        """When the setting is read, as written."""
        return self._read

    def input_mode(self) -> dict[str, str]:
        """The input mode it states, as the engine reads it."""
        return {"mode": "setting_only", "read": self._read}

    def __eq__(self, other: object) -> bool:
        return isinstance(other, SettingOnly) and other._read == self._read

    def __hash__(self) -> int:
        return hash((SettingOnly, self._read))

    def __repr__(self) -> str:
        return "SettingOnly()" if self._read == "at_setup" else f"SettingOnly(read={self._read!r})"


@final
class WireOnly(_InputModeAnnotation):
    """A field that takes only a wire, never a stored value: `Annotated[float, WireOnly()]`."""

    __slots__ = ()

    def input_mode(self) -> dict[str, str]:
        """The input mode it states, as the engine reads it."""
        return {"mode": "wire_only"}


@final
class PublishedOnAnOutput(_InputModeAnnotation):
    """A field whose value its node also publishes on an output of the field's name, which only
    the runtime's own nodes declare: a `GraphInput`'s `value`."""

    __slots__ = ()


@final
class InputValueLifetime:
    """How long a field's value lasts: `InputValueLifetime("kept")` keeps it in the project's
    store, and `InputValueLifetime("reset")` forgets its live value at Stop."""

    __slots__ = ("_lifetime",)

    def __init__(self, lifetime: InputValueLifetimeName) -> None:
        lifetimes = get_args(InputValueLifetimeName.__value__)
        if lifetime not in lifetimes:
            raise ValueError(
                f"InputValueLifetime(...) takes one of {', '.join(repr(each) for each in lifetimes)}, "
                f"got {lifetime!r}."
            )
        self._lifetime: InputValueLifetimeName = lifetime

    @property
    def lifetime(self) -> InputValueLifetimeName:
        """The lifetime as written."""
        return self._lifetime

    def __eq__(self, other: object) -> bool:
        return isinstance(other, InputValueLifetime) and other._lifetime == self._lifetime

    def __hash__(self) -> int:
        return hash((InputValueLifetime, self._lifetime))

    def __repr__(self) -> str:
        return f"InputValueLifetime({self._lifetime!r})"


@final
class StreamDelivery:
    """How a Stream input reads its messages: `StreamDelivery("ordered")` reads each in turn,
    `StreamDelivery("newest")` skips to the latest. `borrow_quota` is how many messages each link
    into it may hold beyond its mailbox. Written only inside `InputPin[Annotated[T, ...]]`."""

    __slots__ = ("_profile", "_borrow_quota")

    def __init__(self, profile: StreamDeliveryProfile, *, borrow_quota: Optional[int] = None) -> None:
        profiles = get_args(StreamDeliveryProfile.__value__)
        if profile not in profiles:
            raise ValueError(
                f"StreamDelivery(...) takes one of {', '.join(repr(each) for each in profiles)}, "
                f"got {profile!r}."
            )
        if borrow_quota is not None and (
            isinstance(borrow_quota, bool) or not isinstance(borrow_quota, int) or borrow_quota < 0
        ):
            raise ValueError(
                f"StreamDelivery(borrow_quota=...) takes a whole number of 0 or more, got "
                f"{borrow_quota!r}."
            )
        self._profile: StreamDeliveryProfile = profile
        self._borrow_quota = borrow_quota

    @property
    def profile(self) -> StreamDeliveryProfile:
        """The delivery profile as written."""
        return self._profile

    @property
    def borrow_quota(self) -> Optional[int]:
        """The borrow quota as written; `None` when it states none."""
        return self._borrow_quota

    def __eq__(self, other: object) -> bool:
        return (
            isinstance(other, StreamDelivery)
            and other._profile == self._profile
            and other._borrow_quota == self._borrow_quota
        )

    def __hash__(self) -> int:
        return hash((StreamDelivery, self._profile, self._borrow_quota))

    def __repr__(self) -> str:
        if self._borrow_quota is None:
            return f"StreamDelivery({self._profile!r})"
        return f"StreamDelivery({self._profile!r}, borrow_quota={self._borrow_quota!r})"


INPUT_DECLARATION_ANNOTATION_CLASSES: Final[tuple[type, ...]] = (
    SettingOnly,
    WireOnly,
    PublishedOnAnOutput,
    InputValueLifetime,
    StreamDelivery,
)
"""Each annotation here, which only a class node's input takes."""
