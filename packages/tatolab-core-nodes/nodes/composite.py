# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Picture nodes that combine two pictures into one."""

from __future__ import annotations

from tatolab.stream import ImageFrame, node


@node.shader(
    """
float4 Mix(ImageInput a, ImageInput b, float amount)
{
    return lerp(a.pixel, b.sample(a.uv), saturate(amount));
}
""",
    description="Crossfade between two pictures: all of a at 0, all of b at 1.",
    category="Composite",
)
def Mix(a: ImageFrame, b: ImageFrame, amount: float = 0.5) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 Over(ImageInput background, ImageInput foreground, float opacity)
{
    float4 top = foreground.sample(background.uv);
    float cover = top.a * saturate(opacity);
    float background_shown = background.pixel.a * (1.0 - cover);
    float alpha = cover + background_shown;
    float3 colour = top.rgb * cover + background.pixel.rgb * background_shown;
    return float4(alpha > 0.0 ? colour / alpha : float3(0.0, 0.0, 0.0), alpha);
}
""",
    description="Lay a foreground over a background, showing the background wherever the foreground is see-through.",
    category="Composite",
)
def Over(  # noqa: N802
    background: ImageFrame, foreground: ImageFrame, opacity: float = 1.0
) -> ImageFrame: ...


@node.shader(
    """
float4 BlendAdd(ImageInput a, ImageInput b, float amount)
{
    float4 top = b.sample(a.uv);
    return float4(saturate(a.pixel.rgb + top.rgb * top.a * amount), a.pixel.a);
}
""",
    description="Add b's light onto a. Black adds nothing, so it suits glows, sparks and projections.",
    category="Composite",
)
def BlendAdd(a: ImageFrame, b: ImageFrame, amount: float = 1.0) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 BlendMultiply(ImageInput a, ImageInput b, float amount)
{
    float4 top = b.sample(a.uv);
    return float4(lerp(a.pixel.rgb, a.pixel.rgb * top.rgb, saturate(amount) * top.a), a.pixel.a);
}
""",
    description="Darken a by b. White changes nothing, so it suits shadows and textures.",
    category="Composite",
)
def BlendMultiply(a: ImageFrame, b: ImageFrame, amount: float = 1.0) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 BlendScreen(ImageInput a, ImageInput b, float amount)
{
    float4 top = b.sample(a.uv);
    float3 screened = 1.0 - (1.0 - a.pixel.rgb) * (1.0 - top.rgb);
    return float4(lerp(a.pixel.rgb, screened, saturate(amount) * top.a), a.pixel.a);
}
""",
    description="Lighten a by b, more gently than Blend Add and never past white.",
    category="Composite",
)
def BlendScreen(a: ImageFrame, b: ImageFrame, amount: float = 1.0) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 BlendDifference(ImageInput a, ImageInput b, float amount)
{
    float4 top = b.sample(a.uv);
    return float4(lerp(a.pixel.rgb, abs(a.pixel.rgb - top.rgb), saturate(amount) * top.a), a.pixel.a);
}
""",
    description="Show where two pictures differ: black where they match, bright where they do not.",
    category="Composite",
)
def BlendDifference(a: ImageFrame, b: ImageFrame, amount: float = 1.0) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 Mask(ImageInput image, ImageInput mask, float invert)
{
    float4 shape = mask.sample(image.uv);
    float keep = dot(shape.rgb, float3(0.2126, 0.7152, 0.0722)) * shape.a;
    keep = invert > 0.5 ? 1.0 - keep : keep;
    return float4(image.pixel.rgb, image.pixel.a * keep);
}
""",
    description="Cut a picture to a shape: it stays where the mask is white and turns see-through where the mask is black.",
    category="Composite",
)
def Mask(image: ImageFrame, mask: ImageFrame, invert: float = 0.0) -> ImageFrame: ...  # noqa: N802


@node.shader(
    """
float4 Wipe(ImageInput a, ImageInput b, float amount, float angle, float softness)
{
    float2 direction = float2(cos(angle * 6.2831853), sin(angle * 6.2831853));
    float along = dot(a.uv - 0.5, direction) / (abs(direction.x) + abs(direction.y)) + 0.5;
    float edge = max(softness, 0.0001);
    float reveal = smoothstep(along - edge, along, saturate(amount) * (1.0 + edge) - edge);
    return lerp(a.pixel, b.sample(a.uv), reveal);
}
""",
    description="Slide from picture a to picture b along a moving edge. Angle is in turns; softness feathers the edge.",
    category="Composite",
)
def Wipe(  # noqa: N802
    a: ImageFrame,
    b: ImageFrame,
    amount: float = 0.5,
    angle: float = 0.0,
    softness: float = 0.02,
) -> ImageFrame: ...
