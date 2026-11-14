# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Picture nodes that read a pixel's neighbours: blur, edges, keys and looks."""

from __future__ import annotations

from tatolab.stream import ImageFrame, node


@node.meta(
    category="Filters",
    description="Soften a picture. Radius is a fraction of its height: 0.01 is gentle, 0.05 is heavy. Chain two for a smoother blur.",
)
@node.shader(
    """
float4 Blur(ImageInput image, float radius)
{
    float2 reach = float2(radius, radius) * float2(0.5625, 1.0);
    float4 sum = image.pixel * 0.16;
    for (int tap = 0; tap < 12; tap++)
    {
        float angle = 6.2831853 * float(tap) / 12.0;
        float ring = (tap % 2 == 0) ? 1.0 : 0.5;
        sum += image.sample(image.uv + float2(cos(angle), sin(angle)) * reach * ring) * 0.07;
    }
    return sum;
}
""",
)
def Blur(image: ImageFrame, radius: float = 0.01) -> ImageFrame: ...  # noqa: N802


@node.meta(category="Filters", description="Crisp up edges and fine detail.")
@node.shader(
    """
float4 Sharpen(ImageInput image, float amount, float radius)
{
    float2 reach = float2(radius, radius) * float2(0.5625, 1.0);
    float4 around = image.sample(image.uv + float2(reach.x, 0.0)) + image.sample(image.uv - float2(reach.x, 0.0))
        + image.sample(image.uv + float2(0.0, reach.y)) + image.sample(image.uv - float2(0.0, reach.y));
    float3 color = image.pixel.rgb + (image.pixel.rgb - around.rgb * 0.25) * amount;
    return float4(saturate(color), image.pixel.a);
}
""",
)
def Sharpen(  # noqa: N802
    image: ImageFrame, amount: float = 1.0, radius: float = 0.002
) -> ImageFrame: ...


@node.meta(
    category="Filters",
    description="Draw only the outlines in a picture: white lines where brightness changes, black elsewhere.",
)
@node.shader(
    """
float EdgeDetect_luma(float4 pixel)
{
    return dot(pixel.rgb, float3(0.2126, 0.7152, 0.0722));
}

float4 EdgeDetect(ImageInput image, float strength, float radius)
{
    float2 reach = float2(radius, radius) * float2(0.5625, 1.0);
    float left = EdgeDetect_luma(image.sample(image.uv - float2(reach.x, 0.0)));
    float right = EdgeDetect_luma(image.sample(image.uv + float2(reach.x, 0.0)));
    float up = EdgeDetect_luma(image.sample(image.uv - float2(0.0, reach.y)));
    float down = EdgeDetect_luma(image.sample(image.uv + float2(0.0, reach.y)));
    float edge = saturate(length(float2(right - left, down - up)) * strength);
    return float4(edge, edge, edge, image.pixel.a);
}
""",
)
def EdgeDetect(  # noqa: N802
    image: ImageFrame, strength: float = 4.0, radius: float = 0.002
) -> ImageFrame: ...


@node.meta(
    category="Filters",
    description="Turn a picture into big square blocks. Fewer cells across means bigger blocks.",
)
@node.shader(
    """
float4 Pixelate(ImageInput image, float cells_across)
{
    float2 cells = float2(max(cells_across, 1.0), max(cells_across, 1.0) * 0.5625);
    return image.sample((floor(image.uv * cells) + 0.5) / cells);
}
""",
)
def Pixelate(image: ImageFrame, cells_across: float = 64.0) -> ImageFrame: ...  # noqa: N802


@node.meta(
    category="Filters",
    description="Bend a picture using another one as a map: the map's red pushes pixels sideways, its green up and down. Try Noise as the map.",
)
@node.shader(
    """
float4 Displace(ImageInput image, ImageInput map, float amount)
{
    float2 push = (map.sample(image.uv).rg - 0.5) * amount;
    return image.sample(image.uv + push);
}
""",
)
def Displace(image: ImageFrame, map: ImageFrame, amount: float = 0.1) -> ImageFrame: ...  # noqa: N802, A002


@node.meta(
    category="Filters",
    description="Pull the red and blue apart for a glitchy, out-of-register look. Angle is in turns: 0.25 is straight up.",
)
@node.shader(
    """
float4 RgbSplit(ImageInput image, float amount, float angle)
{
    float2 offset = float2(cos(angle * 6.2831853), sin(angle * 6.2831853)) * amount;
    float red = image.sample(image.uv + offset).r;
    float blue = image.sample(image.uv - offset).b;
    return float4(red, image.pixel.g, blue, image.pixel.a);
}
""",
)
def RgbSplit(image: ImageFrame, amount: float = 0.005, angle: float = 0.0) -> ImageFrame: ...  # noqa: N802


@node.meta(
    category="Filters",
    description="Darken the corners of a picture to draw the eye to the middle.",
)
@node.shader(
    """
float4 Vignette(ImageInput image, float amount, float size, float softness)
{
    float distance_from_center = length((image.uv - 0.5) * float2(1.0, 0.5625)) * 2.0;
    float shade = smoothstep(size, size + max(softness, 0.0001), distance_from_center);
    return float4(image.pixel.rgb * (1.0 - shade * saturate(amount)), image.pixel.a);
}
""",
)
def Vignette(  # noqa: N802
    image: ImageFrame, amount: float = 0.6, size: float = 0.5, softness: float = 0.5
) -> ImageFrame: ...


@node.meta(
    category="Filters",
    description="Make one colour see-through, such as a green screen. Layer the result over a background with Over.",
)
@node.shader(
    """
float4 ChromaKey(ImageInput image, float key_red, float key_green, float key_blue, float tolerance, float softness)
{
    float3 key = float3(key_red, key_green, key_blue);
    float2 pixel_chroma = image.pixel.rg - image.pixel.b;
    float2 key_chroma = key.rg - key.b;
    float distance_from_key = length(pixel_chroma - key_chroma);
    float keep = smoothstep(tolerance, tolerance + max(softness, 0.0001), distance_from_key);
    return float4(image.pixel.rgb, image.pixel.a * keep);
}
""",
)
def ChromaKey(  # noqa: N802
    image: ImageFrame,
    key_red: float = 0.0,
    key_green: float = 1.0,
    key_blue: float = 0.0,
    tolerance: float = 0.4,
    softness: float = 0.1,
) -> ImageFrame: ...


@node.meta(
    category="Filters",
    description="Make the dark parts of a picture see-through, keeping the bright parts.",
)
@node.shader(
    """
float4 LumaKey(ImageInput image, float threshold, float softness)
{
    float luma = dot(image.pixel.rgb, float3(0.2126, 0.7152, 0.0722));
    float keep = smoothstep(threshold, threshold + max(softness, 0.0001), luma);
    return float4(image.pixel.rgb, image.pixel.a * keep);
}
""",
)
def LumaKey(  # noqa: N802
    image: ImageFrame, threshold: float = 0.2, softness: float = 0.1
) -> ImageFrame: ...
