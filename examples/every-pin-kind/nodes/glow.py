# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A shader node: its pictures are Textures and each of its numbers a 32-bit Float."""

from __future__ import annotations

from tatolab.stream import Texture, node


@node.meta(
    category="Every pin kind",
    description="Brighten the picture by glow and push it toward orange by warmth.",
)
@node.shader(
    """
float4 Glow(ImageInput image, float glow, float warmth)
{
    float3 warmed = image.pixel.rgb * (1.0 + float3(warmth, 0.4 * warmth, -warmth));
    return float4(saturate(warmed + glow), image.pixel.a);
}
""",
)
def Glow(
    image: Texture,
    glow: float = 0.0,
    warmth: float = 0.0,
) -> Texture: ...
