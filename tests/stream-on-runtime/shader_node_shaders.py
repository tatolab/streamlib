# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The shader nodes the shader-node scenarios run: Slang the runtime compiles, with no Python behind it."""

from tatolab.stream import ImageFrame, node

TINT_AMOUNT = 0.5


@node.shader(
    """
float4 tint(ImageInput image, float amount)
{
    return float4(image.pixel.rgb * amount, image.pixel.a);
}
"""
)
def tint(image: ImageFrame, amount: float = TINT_AMOUNT) -> ImageFrame: ...


@node.shader(
    """
float4 invert(ImageInput image)
{
    return float4(1.0 - image.pixel.rgb, image.pixel.a);
}
"""
)
def invert(image: ImageFrame) -> ImageFrame: ...


@node.shader(
    """
float4 swap_red_and_blue(ImageInput image)
{
    return image.pixel.bgra;
}
"""
)
def swap_red_and_blue(image: ImageFrame) -> ImageFrame: ...


@node.shader(
    """
float4 horizontal_blur(ImageInput image)
{
    float2 one_column = float2(1.0 / 64.0, 0.0);
    return (image.sample(image.uv - one_column) + image.sample(image.uv + one_column)) * 0.5;
}
"""
)
def horizontal_blur(image: ImageFrame) -> ImageFrame: ...


@node.shader(
    """
float4 red_ramp_of_the_extent_of(ImageInput extent_from)
{
    return float4(extent_from.uv.x, 0.0, 0.0, 1.0);
}
"""
)
def red_ramp_of_the_extent_of(extent_from: ImageFrame) -> ImageFrame: ...
