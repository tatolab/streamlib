# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each node declaration form and each import the typed grammar retired, with what replaces it.

A retired form is refused where it is written: a retired decorator when it is applied, a retired
import when it is imported. The declaration check reports each one as an `error` in the same
words, found where the source writes it, so a file holding several shows every one.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Final, Optional

__all__ = [
    "A_NODE_WRITTEN_AS_ONE_FUNCTION",
    "RETIRED_DECLARATION_FINDING_CODE",
    "RETIRED_DECLARATIONS",
    "RETIRED_IMPORTS",
    "RetiredDeclaration",
    "RetiredImport",
    "RETIRED_IMPORTS_BY_NAME",
    "RetiredImportRefusal",
    "the_words_refusing_a_retired_declaration",
    "the_words_refusing_a_retired_import",
]

RETIRED_DECLARATION_FINDING_CODE: Final = "declaration_retired"
"""The finding code of a node declared in a retired form, or of a retired import."""


@dataclass(frozen=True, slots=True)
class RetiredDeclaration:
    """One retired form: its decorator, as `tatolab.stream` names it, and what replaces it."""

    decorator: str
    """The decorator's dotted name in `tatolab.stream`, `tatolab.stream.node.function`."""

    spelling: str
    """The decorator as an author writes it, `@node.function`."""

    replaced_by: str
    """What replaces it, said as the fix in the refusal's words."""

    on_a_class_only: bool = False
    """Whether only the decorator on a class is retired: `@node` on a class is, on a function
    it is not."""


@dataclass(frozen=True, slots=True)
class RetiredImport:
    """One name `tatolab.stream` no longer has, and what replaces it."""

    name: str
    """The name as it was imported, `Input`."""

    replaced_by: str
    """What replaces it, said as the fix in the refusal's words."""


class RetiredImportRefusal(ImportError):
    """An import of a name `tatolab.stream` retired, refused with its replacement."""

    def __init__(self, retired: RetiredImport) -> None:
        super().__init__(the_words_refusing_a_retired_import(retired), name="tatolab.stream")
        self.retired: RetiredImport = retired
        """The retired name the import named."""


A_NODE_WRITTEN_AS_ONE_FUNCTION: Final = (
    "`@node(execution=\"reactive\")` on a `def` whose keyword-only parameters are its typed "
    "pins and inputs: `video: InputPin[Texture]` for a stream it reads, `result: "
    "OutputPin[Texture]` for one it writes, `radius: Input[float] = Input(4.0)` for a value it "
    "takes, and `ctx: RuntimeContextFullAccess` for its context. Its body runs once when the "
    "node starts, doing what `setup` did, and returns `each_frame`, a `def each_frame(ctx): ...` "
    "the node calls in place of `process`; an object with a `close()` that a "
    "`computed(lambda: ...)` builds is closed when the node stops, in place of `teardown`"
)
"""The node form that replaces every retired one, said as the fix in a refusal's words."""

RETIRED_DECLARATIONS: Final = (
    RetiredDeclaration(
        decorator="tatolab.stream.node.function",
        spelling="@node.function",
        replaced_by=(
            "bare `@node`, with each parameter typed as its pin, `radius: float = 4.0`, and the "
            "return typed as the output"
        ),
    ),
    RetiredDeclaration(
        decorator="tatolab.stream.node",
        spelling="@node",
        replaced_by=A_NODE_WRITTEN_AS_ONE_FUNCTION,
        on_a_class_only=True,
    ),
    RetiredDeclaration(
        decorator="tatolab.stream.node.process",
        spelling="@node.process",
        replaced_by=(
            f"{A_NODE_WRITTEN_AS_ONE_FUNCTION}; `execution`, `interval_ms`, `scheduling` and "
            f"`paced_by_real_time` go on `@node(...)`"
        ),
    ),
)
"""Every retired form, by its decorator."""

_A_PIN_TYPED_BY_WHAT_IT_CARRIES: Final = (
    "type each pin by what it carries, `InputPin[Texture]`, `OutputPin[Audio]` or `float`"
)

RETIRED_IMPORTS: Final = (
    RetiredImport(name="Output", replaced_by=f"`OutputPin[...]`, a parameter of {A_NODE_WRITTEN_AS_ONE_FUNCTION}"),
    RetiredImport(name="PortFamily", replaced_by=_A_PIN_TYPED_BY_WHAT_IT_CARRIES),
    RetiredImport(name="Image", replaced_by="`Texture`, the pin type of a picture"),
    RetiredImport(
        name="SettingOnly",
        replaced_by=(
            "`Input[...]`, a value input every node reads live; a value built from it once is "
            "a `computed(lambda: ...)` that builds again when it changes"
        ),
    ),
    RetiredImport(name="Array", replaced_by="`Tensor`, the pin type of an array of numbers"),
    RetiredImport(
        name="Channels",
        replaced_by="`Audio` for sound, or `float`, `int` or `bool` for one number",
    ),
    RetiredImport(
        name="Packets",
        replaced_by="`EncodedVideo` or `EncodedAudio`, the pin types of encoded media",
    ),
    RetiredImport(name="ImageFrame", replaced_by="`Texture`, the picture a Texture pin reads"),
    RetiredImport(
        name="ImageFrameToWrite",
        replaced_by="`TextureToWrite`, the picture a Texture pin writes from a pooled texture",
    ),
    RetiredImport(name="ChannelsBlock", replaced_by="`Audio`, the samples an Audio pin reads"),
    RetiredImport(
        name="PacketsUnit",
        replaced_by="`EncodedVideo` or `EncodedAudio`, the unit an encoded media pin reads",
    ),
    RetiredImport(name="ArrayFrame", replaced_by="`Tensor`, the arrays a Tensor pin reads"),
    RetiredImport(
        name="ArrayFrameCpuAttribute",
        replaced_by="`TensorCpuAttribute`, one CPU attribute of a Tensor a read returned",
    ),
    RetiredImport(
        name="ArrayFrameGpuAttribute",
        replaced_by="`TensorGpuAttribute`, one GPU attribute of a Tensor a read returned",
    ),
    RetiredImport(
        name="ProjectAssetUuid",
        replaced_by='a pin typed `Asset`, narrowed by `Annotated[Asset, AssetMedia("image")]`',
    ),
    RetiredImport(
        name="ProjectAssetMedia",
        replaced_by='`AssetMedia("image")`, the media an `Asset` pin takes',
    ),
    RetiredImport(
        name="Records",
        replaced_by="a frozen dataclass, a bundle pin whose members are its fields",
    ),
    RetiredImport(
        name="PortNamesFromConfig",
        replaced_by="a pin group, a frozen dataclass whose fields are the pins of one slot",
    ),
    RetiredImport(
        name="from_config",
        replaced_by="a pin group, a frozen dataclass whose fields are the pins of one slot",
    ),
)
"""Every name `tatolab.stream` retired, by the name an import named."""

RETIRED_IMPORTS_BY_NAME: Final = {retired.name: retired for retired in RETIRED_IMPORTS}
"""Every retired name, by itself."""


def the_words_refusing_a_retired_declaration(
    node_type: Optional[str], retired: RetiredDeclaration
) -> str:
    """What refuses `node_type` declared in the `retired` form, with the fix; `None` for a node
    the form was not yet applied to."""
    declared = "A node" if node_type is None else f"`{node_type}`"
    form = f"a class under `{retired.spelling}`" if retired.on_a_class_only else f"`{retired.spelling}`"
    return f"{declared} is declared with {form}, which is retired. To fix: {retired.replaced_by}."


def the_words_refusing_a_retired_import(retired: RetiredImport) -> str:
    """What refuses an import of the `retired` name, with the fix."""
    return f"`tatolab.stream.{retired.name}` is retired. To fix: {retired.replaced_by}."
