# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The lens: shader nodes that turn the HDR picture into a filmed one.

Every node here is a `@node.shader`, Slang the runtime compiles, fuses and
runs on the GPU once per picture, and every number is a pin a number node can
drive. A shader node's Image is 8-bit, so the one that carries light brighter
than white, GodRays, packs it as RGBM (colour over a shared multiplier in
alpha), the encoding games use to keep HDR in an 8-bit target; LensComposite
unpacks it, adds bloom, streaks, rays and ghosts, and tone-maps with ACES.
"""

from __future__ import annotations

from tatolab.stream import ImageFrame, node

# Shared by the RGBM writer and reader: the brightest value an RGBM pixel holds.
RGBM_RANGE = 8.0


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="Screen-space god rays: march from each pixel toward the sun through the sky the skyline leaves open. Out as RGBM HDR.",
)
@node.shader(
    """
float4 GodRays(ImageInput scene, float sun_x, float sun_y, float density, float decay, float weight, float visible)
{
    float2 sun = float2(sun_x, sun_y);
    float2 to_sun = (sun - scene.uv) * density / 48.0;
    float2 uv = scene.uv;
    float illumination = 1.0;
    float3 rays = float3(0.0, 0.0, 0.0);
    for (int tap = 0; tap < 48; tap++)
    {
        uv += to_sun;
        float4 here = scene.sample(uv);
        // Only open sky lets light through: alpha carries the ray's distance.
        float open = step(900.0, here.a);
        float3 light = min(here.rgb, float3(12.0, 12.0, 12.0)) * open;
        float hot = max(light.r, max(light.g, light.b));
        rays += light * smoothstep(1.5, 5.0, hot) * illumination * weight;
        illumination *= decay;
    }
    rays *= visible / 48.0;
    float m = saturate(max(rays.r, max(rays.g, max(rays.b, 1e-6))) / 8.0);
    m = ceil(m * 255.0) / 255.0;
    return float4(saturate(rays / (m * 8.0 + 1e-6)), m);
}
""",
)
def GodRays(  # noqa: N802
    scene: ImageFrame,
    sun_x: float = 0.5,
    sun_y: float = 0.3,
    density: float = 0.9,
    decay: float = 0.965,
    weight: float = 0.3,
    visible: float = 1.0,
) -> ImageFrame: ...


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="Composite the HDR scene with bloom, the anamorphic streak, god rays and lens ghosts, then tone-map with ACES to the screen.",
)
@node.shader(
    """
float3 LensComposite_aces(float3 x)
{
    // Narkowicz's fit of the ACES filmic curve.
    return saturate((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14));
}

float3 LensComposite_srgb(float3 c)
{
    return lerp(c * 12.92, 1.055 * pow(c, 1.0 / 2.4) - 0.055, step(0.0031308, c));
}

float4 LensComposite(ImageInput scene, ImageInput bloom, ImageInput streak, ImageInput rays,
                     float exposure, float bloom_strength, float streak_strength, float rays_strength,
                     float ghost_strength, float sun_x, float sun_y)
{
    float3 colour = scene.pixel.rgb;
    float3 glow = bloom.sample(scene.uv).rgb;
    colour += glow * bloom_strength;
    // The anamorphic streak, cooled toward cyan the way a coated anamorphic lens draws it.
    float3 streak_light = streak.sample(scene.uv).rgb;
    colour += streak_light * float3(0.55, 0.85, 1.25) * streak_strength;
    float4 packed = rays.sample(scene.uv);
    float3 shafts = packed.rgb * packed.a * 8.0;
    colour += shafts * float3(1.0, 0.55, 0.62) * rays_strength;
    // Lens ghosts: the bloom mirrored through the centre, each a little bigger and split in colour.
    float2 mirrored = 1.0 - scene.uv;
    float2 toward_centre = 0.5 - mirrored;
    for (int ghost = 0; ghost < 4; ghost++)
    {
        float scale = 0.35 + 0.42 * float(ghost);
        float2 at = 0.5 - toward_centre * scale;
        float fade = pow(saturate(1.0 - length(at - 0.5) * 1.7), 2.0);
        float3 g = float3(bloom.sample(at + toward_centre * 0.012).r,
                          bloom.sample(at).g,
                          bloom.sample(at - toward_centre * 0.012).b);
        float3 tint = ghost == 0 ? float3(1.0, 0.24, 0.55) : (ghost == 1 ? float3(0.18, 0.84, 0.63) : (ghost == 2 ? float3(0.36, 0.18, 0.9) : float3(1.0, 0.84, 0.04)));
        colour += g * tint * fade * ghost_strength;
    }
    // A halo ring around the sun's position on the glass.
    float2 sun = float2(sun_x, sun_y);
    float ring = exp(-pow((length((scene.uv - sun) * float2(0.5625, 1.0)) - 0.16) / 0.012, 2.0));
    colour += float3(0.9, 0.5, 1.0) * ring * ghost_strength * 0.08 * saturate(bloom.sample(sun).g);
    colour = LensComposite_aces(colour * exposure);
    return float4(LensComposite_srgb(colour), 1.0);
}
""",
)
def LensComposite(  # noqa: N802
    scene: ImageFrame,
    bloom: ImageFrame,
    streak: ImageFrame,
    rays: ImageFrame,
    exposure: float = 0.9,
    bloom_strength: float = 0.7,
    streak_strength: float = 0.5,
    rays_strength: float = 0.8,
    ghost_strength: float = 0.25,
    sun_x: float = 0.5,
    sun_y: float = 0.3,
) -> ImageFrame: ...


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="Depth of field with a golden-angle bokeh: each pixel's circle of confusion comes from the scene's distance in alpha.",
)
@node.shader(
    """
float DepthOfField_coc(float distance, float focus, float aperture)
{
    return saturate(abs(distance - focus) / max(distance * focus, 1e-3) * 0.18) * aperture;
}

float4 DepthOfField(ImageInput image, ImageInput depth, float focus_distance, float aperture, float aspect)
{
    float centre_coc = DepthOfField_coc(min(depth.pixel.a, 50.0), focus_distance, aperture);
    float3 sum = image.pixel.rgb;
    float total = 1.0;
    for (int tap = 1; tap < 40; tap++)
    {
        float angle = 2.3999632 * float(tap);
        float radius = sqrt(float(tap) / 40.0);
        float2 offset = float2(cos(angle) * aspect, sin(angle)) * radius * centre_coc;
        float2 at = image.uv + offset;
        float tap_coc = DepthOfField_coc(min(depth.sample(at).a, 50.0), focus_distance, aperture);
        // A tap only spreads into this pixel as far as its own blur reaches.
        float reach = saturate((tap_coc - radius * centre_coc) / max(centre_coc * 0.15, 1e-4) + 0.5);
        float3 c = image.sample(at).rgb;
        float bright = 1.0 + 2.5 * smoothstep(0.7, 1.0, max(c.r, max(c.g, c.b)));
        float w = reach * bright;
        sum += c * w;
        total += w;
    }
    return float4(sum / total, 1.0);
}
""",
)
def DepthOfField(  # noqa: N802
    image: ImageFrame,
    depth: ImageFrame,
    focus_distance: float = 0.4,
    aperture: float = 0.006,
    aspect: float = 1.7778,
) -> ImageFrame: ...


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="Heat haze: the air shimmers over the warm water near the horizon, bending what is behind it.",
)
@node.shader(
    """
float HeatHaze_noise(float2 p)
{
    float2 i = floor(p);
    float2 f = frac(p);
    float2 u = f * f * (3.0 - 2.0 * f);
    float a = frac(sin(dot(i, float2(127.1, 311.7))) * 43758.5453);
    float b = frac(sin(dot(i + float2(1.0, 0.0), float2(127.1, 311.7))) * 43758.5453);
    float c = frac(sin(dot(i + float2(0.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    float d = frac(sin(dot(i + float2(1.0, 1.0), float2(127.1, 311.7))) * 43758.5453);
    return lerp(lerp(a, b, u.x), lerp(c, d, u.x), u.y);
}

float4 HeatHaze(ImageInput image, float seconds, float amount, float horizon_y, float band)
{
    float near_horizon = exp(-pow((image.uv.y - horizon_y) / max(band, 1e-3), 2.0));
    float2 p = float2(image.uv.x * 38.0, image.uv.y * 90.0 - seconds * 2.6);
    float2 wobble = float2(HeatHaze_noise(p) - 0.5, HeatHaze_noise(p + 17.3) - 0.5);
    float2 at = image.uv + wobble * amount * near_horizon * float2(0.6, 1.0);
    return float4(image.sample(at).rgb, 1.0);
}
""",
)
def HeatHaze(  # noqa: N802
    image: ImageFrame,
    seconds: float = 0.0,
    amount: float = 0.004,
    horizon_y: float = 0.45,
    band: float = 0.08,
) -> ImageFrame: ...


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="Radial chromatic aberration: a spectral split that grows toward the frame's edges, like a fast lens wide open.",
)
@node.shader(
    """
float4 ChromaticAberration(ImageInput image, float amount, float aspect)
{
    float2 from_centre = (image.uv - 0.5) * float2(aspect, 1.0);
    float reach = dot(from_centre, from_centre);
    float2 shift = (image.uv - 0.5) * reach * amount;
    float3 sum = float3(0.0, 0.0, 0.0);
    float3 weights = float3(0.0, 0.0, 0.0);
    for (int tap = 0; tap < 7; tap++)
    {
        float t = float(tap) / 6.0;
        // Each tap a slice of the spectrum: red at the outside, violet in.
        float3 spectrum = saturate(float3(1.5 - abs(t - 0.0) * 3.0, 1.5 - abs(t - 0.5) * 3.0, 1.5 - abs(t - 1.0) * 3.0));
        sum += image.sample(image.uv - shift * (t - 0.5) * 2.0).rgb * spectrum;
        weights += spectrum;
    }
    return float4(sum / weights, 1.0);
}
""",
)
def ChromaticAberration(image: ImageFrame, amount: float = 0.02, aspect: float = 0.5625) -> ImageFrame: ...  # noqa: N802


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="The Tato grade: violet in the shadows, cream in the highlights, a filmic S-curve and saturation.",
)
@node.shader(
    """
float4 NeonGrade(ImageInput image, float shadow_magenta, float highlight_peach, float contrast, float saturation)
{
    float3 c = image.pixel.rgb;
    float luma = dot(c, float3(0.2126, 0.7152, 0.0722));
    float3 shadows = float3(0.357, 0.180, 0.902);
    float3 highlights = float3(1.0, 0.957, 0.878);
    c += shadows * shadow_magenta * (1.0 - smoothstep(0.0, 0.45, luma)) * 0.25;
    c = lerp(c, c * highlights * 1.25, highlight_peach * smoothstep(0.55, 1.0, luma));
    c = saturate(lerp(float3(0.5, 0.5, 0.5), c, contrast));
    c = c * c * (3.0 - 2.0 * c) * 0.35 + c * 0.65;
    float grey = dot(c, float3(0.2126, 0.7152, 0.0722));
    c = lerp(float3(grey, grey, grey), c, saturation);
    return float4(saturate(c), 1.0);
}
""",
)
def NeonGrade(  # noqa: N802
    image: ImageFrame,
    shadow_magenta: float = 0.3,
    highlight_peach: float = 0.25,
    contrast: float = 1.08,
    saturation: float = 1.12,
) -> ImageFrame: ...


@node.meta(
    category="Gulf Stream Neon · Lens",
    description="The film finish: animated grain and soft scanlines.",
)
@node.shader(
    """
float FilmFinish_hash(float2 p)
{
    float3 q = frac(float3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return frac((q.x + q.y) * q.z);
}

float4 FilmFinish(ImageInput image, float seconds, float grain, float scanlines, float lines_across)
{
    float3 colour = image.pixel.rgb;
    float row = image.uv.y * lines_across;
    colour *= 1.0 - scanlines * (0.5 + 0.5 * cos(row * 6.2831853));
    float2 cell = floor(image.uv * float2(lines_across * 0.5625, lines_across));
    float noise = FilmFinish_hash(cell + frac(seconds * 7.13) * 917.0) - 0.5;
    float luma = dot(colour, float3(0.2126, 0.7152, 0.0722));
    colour += noise * grain * (0.6 + 0.8 * (1.0 - luma));
    return float4(saturate(colour), 1.0);
}
""",
)
def FilmFinish(  # noqa: N802
    image: ImageFrame,
    seconds: float = 0.0,
    grain: float = 0.03,
    scanlines: float = 0.035,
    lines_across: float = 960.0,
) -> ImageFrame: ...
