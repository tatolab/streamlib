# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Picture nodes that change colour, pixel for pixel, on the GPU."""

from __future__ import annotations

from tatolab.stream import Color, Texture, node


@node.meta(
    category="Color",
    description="Brightness, contrast, gamma and opacity in one node. Brightness adds, contrast multiplies around mid grey.",
)
@node.shader(
    """
float4 Level(ImageInput image, float brightness, float contrast, float gamma, float opacity)
{
    float3 color = (image.pixel.rgb - 0.5) * contrast + 0.5 + brightness;
    color = pow(saturate(color), 1.0 / max(gamma, 0.001));
    return float4(color, image.pixel.a * saturate(opacity));
}
""",
)
def Level(  # noqa: N802
    image: Texture,
    brightness: float = 0.0,
    contrast: float = 1.0,
    gamma: float = 1.0,
    opacity: float = 1.0,
) -> Texture: ...


@node.meta(
    category="Color",
    description="Shift hue around the colour wheel (0 to 1 is one full turn) and scale saturation and brightness.",
)
@node.shader(
    """
float3 HsvAdjust_rgb_to_hsv(float3 color)
{
    float4 k = float4(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    float4 p = lerp(float4(color.bg, k.wz), float4(color.gb, k.xy), step(color.b, color.g));
    float4 q = lerp(float4(p.xyw, color.r), float4(color.r, p.yzx), step(p.x, color.r));
    float d = q.x - min(q.w, q.y);
    float e = 1.0e-10;
    return float3(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

float3 HsvAdjust_hsv_to_rgb(float3 hsv)
{
    float3 p = abs(frac(hsv.xxx + float3(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return hsv.z * lerp(float3(1.0, 1.0, 1.0), saturate(p - 1.0), hsv.y);
}

float4 HsvAdjust(ImageInput image, float hue_shift, float saturation, float value)
{
    float3 hsv = HsvAdjust_rgb_to_hsv(image.pixel.rgb);
    hsv.x = frac(hsv.x + hue_shift);
    hsv.y = saturate(hsv.y * saturation);
    hsv.z = saturate(hsv.z * value);
    return float4(HsvAdjust_hsv_to_rgb(hsv), image.pixel.a);
}
""",
)
def HsvAdjust(  # noqa: N802
    image: Texture, hue_shift: float = 0.0, saturation: float = 1.0, value: float = 1.0
) -> Texture: ...


@node.meta(
    category="Color",
    description="Flip every colour to its opposite, like a film negative. Amount fades the effect in.",
)
@node.shader(
    """
float4 Invert(ImageInput image, float amount)
{
    return float4(lerp(image.pixel.rgb, 1.0 - image.pixel.rgb, saturate(amount)), image.pixel.a);
}
""",
)
def Invert(image: Texture, amount: float = 1.0) -> Texture: ...  # noqa: N802


@node.meta(
    category="Color",
    description="Turn a picture black and white. Amount fades between colour and grey.",
)
@node.shader(
    """
float4 Monochrome(ImageInput image, float amount)
{
    float luma = dot(image.pixel.rgb, float3(0.2126, 0.7152, 0.0722));
    return float4(lerp(image.pixel.rgb, float3(luma, luma, luma), saturate(amount)), image.pixel.a);
}
""",
)
def Monochrome(image: Texture, amount: float = 1.0) -> Texture: ...  # noqa: N802


@node.meta(
    category="Color",
    description="Pure black below a brightness, pure white above it. Softness blurs the cut.",
)
@node.shader(
    """
float4 Threshold(ImageInput image, float threshold, float softness)
{
    float luma = dot(image.pixel.rgb, float3(0.2126, 0.7152, 0.0722));
    float half_softness = max(softness, 0.0001) * 0.5;
    float level = smoothstep(threshold - half_softness, threshold + half_softness, luma);
    return float4(level, level, level, image.pixel.a);
}
""",
)
def Threshold(  # noqa: N802
    image: Texture, threshold: float = 0.5, softness: float = 0.02
) -> Texture: ...


@node.meta(
    category="Color",
    description="Multiply a picture by a colour, like a coloured gel over a light. Amount and the colour's alpha fade the effect in.",
)
@node.shader(
    """
float4 Tint(ImageInput image, float4 color, float amount)
{
    float3 tinted = image.pixel.rgb * color.rgb;
    return float4(lerp(image.pixel.rgb, tinted, saturate(amount) * saturate(color.a)), image.pixel.a);
}
""",
)
def Tint(  # noqa: N802
    image: Texture,
    color: Color = Color(1.0, 0.5, 0.2, 1.0),
    amount: float = 1.0,
) -> Texture: ...


@node.meta(
    category="Color",
    description="Recolour a picture with two colours: one for the shadows, one for the highlights.",
)
@node.shader(
    """
float4 Duotone(ImageInput image, float4 dark, float4 light)
{
    float luma = dot(image.pixel.rgb, float3(0.2126, 0.7152, 0.0722));
    float4 color = lerp(dark, light, luma);
    return float4(color.rgb, image.pixel.a * saturate(color.a));
}
""",
)
def Duotone(  # noqa: N802
    image: Texture,
    dark: Color = Color(0.05, 0.0, 0.3, 1.0),
    light: Color = Color(1.0, 0.8, 0.4, 1.0),
) -> Texture: ...


@node.meta(
    category="Color",
    description="Reduce a picture to a few flat colour steps per channel, for a screen-print look.",
)
@node.shader(
    """
float4 Posterize(ImageInput image, float levels)
{
    float steps = max(floor(levels + 0.5), 2.0) - 1.0;
    return float4(floor(image.pixel.rgb * steps + 0.5) / steps, image.pixel.a);
}
""",
)
def Posterize(image: Texture, levels: float = 4.0) -> Texture: ...  # noqa: N802


@node.meta(
    category="Color",
    description="Recolour a picture through a gradient: dark pixels take the gradient's left edge, bright ones its right.",
)
@node.shader(
    """
float4 Lookup(ImageInput image, ImageInput gradient)
{
    float luma = dot(image.pixel.rgb, float3(0.2126, 0.7152, 0.0722));
    return float4(gradient.sample(float2(saturate(luma), 0.5)).rgb, image.pixel.a);
}
""",
)
def Lookup(image: Texture, gradient: Texture) -> Texture: ...  # noqa: N802
