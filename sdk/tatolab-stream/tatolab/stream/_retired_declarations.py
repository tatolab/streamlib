# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Each node declaration form the typed grammar retires, with what replaces it.

The declaration check answers a node declared in one of these forms with a warning naming its
replacement, or, where the typed grammar has none yet, the piece of it that will bring one.
The source reader finds each form where its decorator is written and judges nothing.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Final

__all__ = ["RETIRED_DECLARATION_FINDING_CODE", "RETIRED_DECLARATIONS", "RetiredDeclaration"]

RETIRED_DECLARATION_FINDING_CODE: Final = "declaration_retired"
"""The finding code of a node declared in a retired form."""


@dataclass(frozen=True, slots=True)
class RetiredDeclaration:
    """One retired form: its decorator, as `tatolab.stream` names it, and the words of its warning."""

    decorator: str
    """The decorator's dotted name in `tatolab.stream`, `tatolab.stream.node.function`."""

    spelling: str
    """The decorator as an author writes it, `@node.function`."""

    replaced_by: str
    """What replaces it, said as the fix in the warning's words."""


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
        decorator="tatolab.stream.node.process",
        spelling="@node.process",
        replaced_by=(
            "a class node whose fields are its typed pins, `video: InputPin[Texture]`, which a "
            "later release brings; keep `@node.process` until then"
        ),
    ),
)
"""Every retired form, by its decorator."""
