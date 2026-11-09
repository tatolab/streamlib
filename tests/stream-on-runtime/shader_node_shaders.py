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
