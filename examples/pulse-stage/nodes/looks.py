# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`glow_look`: a graph generator, placed in a stream as a subgraph whose nodes it writes, and
the two shaders it writes them from.

The look blurs its picture `passes` times, each pass reaching twice as far as the last, and
adds the blur back over the sharp picture, so bright things bloom. `passes` and `strength`
are the generator's promoted settings: changing one on the subgraph's card runs the generator
again and refreshes the nodes inside.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    Description,
    GraphInput,
    GraphOutput,
    ImageFrame,
    NumberRange,
    NumberStep,
    StreamBuilder,
    graph_generator,
    node,
)

#: How far the first pass reaches, as a fraction of the picture's height.
FIRST_PASS_RADIUS = 0.006


@node.meta(
    category="Pulse Stage",
    description="Soften the picture by sampling a ring around each pixel. Radius is a fraction of the picture's height.",
)
@node.shader(
    """
float4 GlowBlur(ImageInput image, float radius)
{
    float2 reach = float2(radius * 0.5625, radius);
    float4 sum = image.pixel * 0.2;
    for (int tap = 0; tap < 16; tap++)
    {
        float angle = 6.2831853 * float(tap) / 16.0;
        sum += image.sample(image.uv + float2(cos(angle), sin(angle)) * reach) * 0.05;
    }
    return sum;
}
""",
)
def GlowBlur(
    image: Annotated[ImageFrame, Description("The picture to soften.")],
    radius: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=0.1),
        NumberStep(0.001),
        Description("How far it spreads, as a share of the frame."),
    ] = 0.01,
) -> ImageFrame: ...


@node.meta(
    category="Pulse Stage",
    description="Add light over a picture: the picture plus light at amount.",
)
@node.shader(
    """
float4 GlowAdd(ImageInput picture, ImageInput light, float amount)
{
    return float4(saturate(picture.pixel.rgb + light.sample(picture.uv).rgb * amount), picture.pixel.a);
}
""",
)
def GlowAdd(
    picture: Annotated[ImageFrame, Description("The sharp picture.")],
    light: Annotated[ImageFrame, Description("The softened light added over it.")],
    amount: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=4.0),
        NumberStep(0.05),
        Description("How much light is added."),
    ] = 1.0,
) -> ImageFrame: ...


@graph_generator
def glow_look(
    stream_builder: StreamBuilder, passes: int = 3, strength: float = 0.9
) -> None:
    """A bloom: the picture into `video_in` blurred `passes` times and added back at `strength`."""
    picture = stream_builder.add(
        GraphInput,
        name="video_in",
        input_values={"port": "video_in", "family": "image"},
    )
    blurred = picture.output("value")
    for pass_index in range(max(1, passes)):
        blur = stream_builder.add(
            GlowBlur,
            name=f"blur_{pass_index + 1}",
            input_values={"radius": FIRST_PASS_RADIUS * 2**pass_index},
        )
        stream_builder.connect(blurred, blur.input("image"))
        blurred = blur.output("result")
    bloom = stream_builder.add(GlowAdd, name="bloom", input_values={"amount": strength})
    stream_builder.connect(picture.output("value"), bloom.input("picture"))
    stream_builder.connect(blurred, bloom.input("light"))
    glowing = stream_builder.add(
        GraphOutput,
        name="video_out",
        input_values={"port": "video_out", "family": "image"},
    )
    stream_builder.connect(bloom.output("result"), glowing.input("value"))
