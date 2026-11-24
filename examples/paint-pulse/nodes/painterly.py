# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`Painterly`: a shader node that repaints any picture as oil strokes.

For each pixel it reads which way the picture's edges run, averages four windows stretched
along that direction, and keeps the calmest (an anisotropic Kuwahara filter), so flat patches
become strokes that follow the forms. Bristle grooves along each stroke and a canvas weave
finish it. Every number is a pin, so it can be driven by any Float, the music included.
"""

from __future__ import annotations

from tatolab.stream import Texture, node


@node.meta(
    category="Paint Pulse",
    description="Repaint the picture as oil strokes that follow its edges, with bristle grooves and a canvas weave.",
)
@node.shader(
    """
float painterly_hash(float2 p)
{
    float3 q = frac(float3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return frac((q.x + q.y) * q.z);
}

float painterly_noise(float2 p)
{
    float2 i = floor(p);
    float2 f = frac(p);
    float2 s = f * f * (3.0 - 2.0 * f);
    return lerp(lerp(painterly_hash(i), painterly_hash(i + float2(1, 0)), s.x),
                lerp(painterly_hash(i + float2(0, 1)), painterly_hash(i + float2(1, 1)), s.x), s.y);
}

float painterly_luma(float3 c)
{
    return dot(c, float3(0.299, 0.587, 0.114));
}

float4 Painterly(ImageInput image, float stroke, float width, float height, float time, float canvas, float amount)
{
    float2 texel = 1.0 / float2(max(width, 1.0), max(height, 1.0));
    float2 px = image.uv * float2(width, height);
    float radius = max(1.5, stroke * 0.5);

    // Which way edges run: a structure tensor over a 3 x 3 neighbourhood of central differences.
    float jxx = 0.0;
    float jyy = 0.0;
    float jxy = 0.0;
    for (int oy = -1; oy <= 1; oy++)
    {
        for (int ox = -1; ox <= 1; ox++)
        {
            float2 at = image.uv + float2(ox, oy) * texel * 2.0;
            float gx = painterly_luma(image.sample(at + float2(texel.x, 0.0)).rgb) - painterly_luma(image.sample(at - float2(texel.x, 0.0)).rgb);
            float gy = painterly_luma(image.sample(at + float2(0.0, texel.y)).rgb) - painterly_luma(image.sample(at - float2(0.0, texel.y)).rgb);
            jxx += gx * gx;
            jyy += gy * gy;
            jxy += gx * gy;
        }
    }
    float angle = 0.5 * atan2(2.0 * jxy, jxx - jyy) + 1.5707963;
    float anisotropy = sqrt((jxx - jyy) * (jxx - jyy) + 4.0 * jxy * jxy) / (jxx + jyy + 1e-5);
    // Where the picture is flat the strokes lean on one slant, wavering slowly with time.
    float slant = 0.45 + 0.25 * sin(time * 0.15 + px.y * 0.004);
    angle = lerp(slant, angle, smoothstep(0.0005, 0.004, jxx + jyy));
    float2 along = float2(cos(angle), sin(angle));
    float2 across = float2(-along.y, along.x);
    float stretch = 1.0 + 1.2 * anisotropy;

    // Four windows either side of the stroke's line, 4 x 4 taps each; the calmest wins, softly.
    float total_weight = 0.0;
    float3 blended = float3(0.0, 0.0, 0.0);
    for (int quadrant = 0; quadrant < 4; quadrant++)
    {
        float sa = (quadrant & 1) == 0 ? 1.0 : -1.0;
        float sc = (quadrant & 2) == 0 ? 1.0 : -1.0;
        float3 sum = float3(0.0, 0.0, 0.0);
        float3 sum_sq = float3(0.0, 0.0, 0.0);
        for (int j = 0; j < 4; j++)
        {
            for (int i = 0; i < 4; i++)
            {
                float2 offset = along * (sa * float(i) / 3.0 * radius * stretch) + across * (sc * float(j) / 3.0 * radius / stretch);
                float3 c = image.sample(image.uv + offset * texel).rgb;
                sum += c;
                sum_sq += c * c;
            }
        }
        float3 mean = sum / 16.0;
        float3 variance = abs(sum_sq / 16.0 - mean * mean);
        float sigma = variance.r + variance.g + variance.b;
        float weight = 1.0 / (1e-4 + pow(sigma * 40.0, 4.0));
        blended += mean * weight;
        total_weight += weight;
    }
    float3 colour = blended / total_weight;

    // Bristles along the stroke, broken every stroke length, and the canvas weave under them.
    float2 stroke_space = float2(dot(px, along) / (stroke * 1.6), dot(px, across) / (stroke * 0.32));
    float grooves = painterly_noise(stroke_space) * 0.6 + painterly_noise(stroke_space * float2(0.35, 3.1)) * 0.4;
    colour *= 0.9 + 0.18 * grooves;
    colour += 0.035 * smoothstep(0.65, 0.95, grooves);
    colour *= 1.0 - canvas * sin(px.x * 1.9) * sin(px.y * 1.9);
    return float4(lerp(image.pixel.rgb, saturate(colour), saturate(amount)), 1.0);
}
""",
)
def Painterly(  # noqa: N802
    image: Texture,
    stroke: float = 14.0,
    width: float = 1280.0,
    height: float = 720.0,
    time: float = 0.0,
    canvas: float = 0.03,
    amount: float = 1.0,
) -> Texture: ...
