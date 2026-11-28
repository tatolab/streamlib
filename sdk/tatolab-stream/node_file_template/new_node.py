# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    node,
)


@node.meta(description="Hands on one picture in every `every_nth` it reads, unchanged.")
@node(execution="reactive")
def NewNode[Frame: Texture](
    *,
    picture_in: InputPin[
        Annotated[Frame, StreamDelivery("ordered"), Description("The pictures this node reads.")]
    ],
    every_nth: Input[
        Annotated[
            int,
            NumberRange(minimum=1, maximum=60),
            Description("How many pictures read make one handed on."),
        ]
    ] = Input(1),
    picture_out: OutputPin[
        Annotated[Frame, Description("One picture in every `every_nth` read from `picture_in`.")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    pictures_read = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal pictures_read
        picture = picture_in.read()
        if picture is None:
            return
        pictures_read += 1
        if pictures_read % every_nth.get() == 0:
            picture_out.write(picture)

    return each_frame
