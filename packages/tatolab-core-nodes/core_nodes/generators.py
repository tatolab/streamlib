# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Picture nodes that draw from nothing but numbers.

Each draws at the size of the picture wired into `size_from` and reads none
of its pixels; a `SolidColor` is the usual one.
"""

from __future__ import annotations

from tatolab.stream import Color, Texture, Vector2, node


@node.meta(
    category="Generators",
    description="A smooth fade from one colour to another across the frame. Angle is in turns: 0 runs left to right, 0.25 top to bottom.",
)
@node.shader(
    """
float4 Gradient(ImageInput size_from, float angle, float4 start, float4 end)
{
    float2 direction = float2(cos(angle * 6.2831853), sin(angle * 6.2831853));
    float along = saturate(dot(size_from.uv - 0.5, direction) / (abs(direction.x) + abs(direction.y)) + 0.5);
    return lerp(start, end, along);
}
""",
)
def Gradient(  # noqa: N802
    size_from: Texture,
    angle: float = 0.0,
    start: Color = Color(0.0, 0.0, 0.0, 1.0),
    end: Color = Color(1.0, 1.0, 1.0, 1.0),
) -> Texture: ...


@node.meta(
    category="Generators",
    description="A round fade from an inner colour at the centre to an outer colour at the radius.",
)
@node.shader(
    """
float4 RadialGradient(ImageInput size_from, float2 center, float radius, float4 inner, float4 outer)
{
    float distance_from_center = length((size_from.uv - center) * float2(1.0, 0.5625));
    float along = saturate(distance_from_center / max(radius, 0.0001));
    return lerp(inner, outer, along);
}
""",
)
def RadialGradient(  # noqa: N802
    size_from: Texture,
    center: Vector2 = Vector2(0.5, 0.5),
    radius: float = 0.5,
    inner: Color = Color(1.0, 1.0, 1.0, 1.0),
    outer: Color = Color(0.0, 0.0, 0.0, 1.0),
) -> Texture: ...


@node.meta(
    category="Generators",
    description="Soft grey clouds. Wire a Timer or a Speed into time to make them drift. A good map for Displace and a good mask.",
)
@node.shader(
    """
float Noise_hash(float3 lattice_point)
{
    lattice_point = frac(lattice_point * 0.3183099 + 0.1);
    lattice_point *= 17.0;
    return frac(lattice_point.x * lattice_point.y * lattice_point.z * (lattice_point.x + lattice_point.y + lattice_point.z));
}

float Noise_value(float3 position)
{
    float3 cell = floor(position);
    float3 within = frac(position);
    within = within * within * (3.0 - 2.0 * within);
    float near = lerp(lerp(Noise_hash(cell), Noise_hash(cell + float3(1.0, 0.0, 0.0)), within.x),
                      lerp(Noise_hash(cell + float3(0.0, 1.0, 0.0)), Noise_hash(cell + float3(1.0, 1.0, 0.0)), within.x), within.y);
    float far = lerp(lerp(Noise_hash(cell + float3(0.0, 0.0, 1.0)), Noise_hash(cell + float3(1.0, 0.0, 1.0)), within.x),
                     lerp(Noise_hash(cell + float3(0.0, 1.0, 1.0)), Noise_hash(cell + float3(1.0, 1.0, 1.0)), within.x), within.y);
    return lerp(near, far, within.z);
}

float4 Noise(ImageInput size_from, float scale, float time, float detail, float contrast)
{
    float3 position = float3(size_from.uv * float2(1.0, 0.5625) * scale, time);
    float sum = 0.0;
    float weight = 0.5;
    float total = 0.0;
    for (int octave = 0; octave < 5; octave++)
    {
        float strength = weight * (octave == 0 ? 1.0 : saturate(detail));
        sum += Noise_value(position) * strength;
        total += strength;
        position *= 2.0;
        weight *= 0.5;
    }
    float level = saturate((sum / max(total, 0.0001) - 0.5) * contrast + 0.5);
    return float4(level, level, level, 1.0);
}
""",
)
def Noise(  # noqa: N802
    size_from: Texture,
    scale: float = 6.0,
    time: float = 0.0,
    detail: float = 0.6,
    contrast: float = 1.5,
) -> Texture: ...


@node.meta(
    category="Generators",
    description="Black and white squares, for lining up a projector or testing an effect.",
)
@node.shader(
    """
float4 Checkerboard(ImageInput size_from, float cells_across, float2 scroll)
{
    float2 cell = floor((size_from.uv * float2(1.0, 0.5625) + scroll) * max(cells_across, 1.0));
    float level = fmod(abs(cell.x + cell.y), 2.0);
    return float4(level, level, level, 1.0);
}
""",
)
def Checkerboard(  # noqa: N802
    size_from: Texture, cells_across: float = 16.0, scroll: Vector2 = Vector2(0.0, 0.0)
) -> Texture: ...


@node.meta(
    category="Generators",
    description="Evenly spaced bars at any angle. Wire a Ramp into scroll to make them march.",
)
@node.shader(
    """
float4 Stripes(ImageInput size_from, float count, float angle, float scroll, float width, float softness)
{
    float2 direction = float2(cos(angle * 6.2831853), sin(angle * 6.2831853));
    float along = frac(dot((size_from.uv - 0.5) * float2(1.0, 0.5625), direction) * count + scroll);
    float edge = max(softness, 0.0001);
    float level = smoothstep(0.0, edge, along) * (1.0 - smoothstep(width, width + edge, along));
    return float4(level, level, level, 1.0);
}
""",
)
def Stripes(  # noqa: N802
    size_from: Texture,
    count: float = 8.0,
    angle: float = 0.0,
    scroll: float = 0.0,
    width: float = 0.5,
    softness: float = 0.02,
) -> Texture: ...


@node.meta(
    category="Generators",
    description="A filled circle on a see-through background. Use it as a spotlight, a dot, or a mask.",
)
@node.shader(
    """
float4 Circle(ImageInput size_from, float2 center, float radius, float softness, float4 color)
{
    float distance_from_center = length((size_from.uv - center) * float2(1.0, 0.5625));
    float cover = 1.0 - smoothstep(radius, radius + max(softness, 0.0001), distance_from_center);
    return float4(color.rgb, cover * saturate(color.a));
}
""",
)
def Circle(  # noqa: N802
    size_from: Texture,
    center: Vector2 = Vector2(0.5, 0.5),
    radius: float = 0.2,
    softness: float = 0.005,
    color: Color = Color(1.0, 1.0, 1.0, 1.0),
) -> Texture: ...


@node.meta(
    category="Generators",
    description="A filled rectangle on a see-through background. Width and height are fractions of the frame.",
)
@node.shader(
    """
float4 Rectangle(ImageInput size_from, float2 center, float width, float height, float softness, float4 color)
{
    float2 from_edge = abs(size_from.uv - center) - float2(width, height) * 0.5;
    float outside = max(from_edge.x, from_edge.y);
    float cover = 1.0 - smoothstep(0.0, max(softness, 0.0001), outside);
    return float4(color.rgb, cover * saturate(color.a));
}
""",
)
def Rectangle(  # noqa: N802
    size_from: Texture,
    center: Vector2 = Vector2(0.5, 0.5),
    width: float = 0.5,
    height: float = 0.3,
    softness: float = 0.005,
    color: Color = Color(1.0, 1.0, 1.0, 1.0),
) -> Texture: ...
