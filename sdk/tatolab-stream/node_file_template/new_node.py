# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

from typing import Annotated

from tatolab.stream import (
    Description,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    node,
)


@node.meta(description="Hands on one picture in every `every_nth` it reads, unchanged.")
@node
class NewNode[Frame: Texture]:
    picture_in: InputPin[
        Annotated[Frame, StreamDelivery("ordered"), Description("The pictures this node reads.")]
    ]
    every_nth: Annotated[
        int,
        NumberRange(minimum=1, maximum=60),
        Description("How many pictures read make one handed on."),
    ] = 1
    picture_out: OutputPin[
        Annotated[Frame, Description("One picture in every `every_nth` read from `picture_in`.")]
    ]

    _pictures_read: int = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        picture = self.picture_in.read()
        if picture is None:
            return
        self._pictures_read += 1
        if self._pictures_read % self.every_nth == 0:
            self.picture_out.write(picture)
