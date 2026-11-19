# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The finish: shader nodes for bloom and film, and the number nodes that drive them.

Every shader here is a `@node.shader`, Slang the runtime compiles and runs on
the GPU once per picture. The number nodes turn stream time into the
soundtrack's beat and the take's beats, so any knob of the finish can be
wired to the music in the graph.
"""

from __future__ import annotations

from dataclasses import dataclass

from tatolab.stream import ImageFrame, node

from nodes.neon_world import smoothstep
from nodes.soundtrack_analysis import soundtrack_curves


@node.shader(
    """
float4 NeonBrightPass(ImageInput image, float threshold, float boost)
{
    float3 colour = image.pixel.rgb;
    float brightness = max(colour.r, max(colour.g, colour.b));
    float keep = smoothstep(threshold, threshold + 0.25, brightness);
    return float4(colour * keep * boost, 1.0);
}
""",
)
def NeonBrightPass(image: ImageFrame, threshold: float = 0.55, boost: float = 1.2) -> ImageFrame: ...


@node.shader(
    """
float4 NeonBloom(ImageInput image, float radius, float aspect)
{
    // A 16-tap disc at `radius` of the frame's height, round on any frame shape.
    float2 reach = float2(radius / max(aspect, 0.01), radius);
    float4 sum = image.pixel * 0.2;
    for (int tap = 0; tap < 16; tap++)
    {
        float angle = 2.3999632 * float(tap);
        float ring = sqrt((float(tap) + 0.5) / 16.0);
        sum += image.sample(image.uv + float2(cos(angle), sin(angle)) * reach * ring) * 0.05;
    }
    return float4(sum.rgb, 1.0);
}
""",
)
def NeonBloom(image: ImageFrame, radius: float = 0.02, aspect: float = 0.5625) -> ImageFrame: ...


@node.shader(
    """
float NeonFilmScan_hash(float2 p)
{
    p = frac(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return frac(p.x * p.y);
}

float4 NeonFilmScan(ImageInput image, float seconds, float grain, float scanlines, float lines_across)
{
    float3 colour = image.pixel.rgb;
    float row = image.uv.y * lines_across;
    colour *= 1.0 - scanlines * (0.5 + 0.5 * cos(row * 6.2831853));
    float noise = NeonFilmScan_hash(image.uv * 1931.0 + frac(seconds * 7.13) * 917.0) - 0.5;
    colour += noise * grain;
    return float4(saturate(colour), 1.0);
}
""",
)
def NeonFilmScan(
    image: ImageFrame,
    seconds: float = 0.0,
    grain: float = 0.035,
    scanlines: float = 0.05,
    lines_across: float = 480.0,
) -> ImageFrame: ...


@dataclass(frozen=True)
class SoundtrackReading:
    """The soundtrack at one instant: how hard the kick hits and how loud the track is."""

    beat: float
    energy: float


@node.function
def SoundtrackBeat(seconds: float = 0.0) -> SoundtrackReading:
    """The soundtrack's kick envelope and loudness, both 0 to 1, `seconds` into the track."""
    beat, energy = soundtrack_curves().at(seconds)
    return SoundtrackReading(beat, energy)


@dataclass(frozen=True)
class TakeReading:
    """Where the take is: how far into the drop, and how far the sun has set."""

    drop: float
    sunset: float


@node.function
def TakeBeats(seconds: float = 0.0) -> TakeReading:
    """The take's sections as 0-to-1 levels: `drop` is up from 10 s to 20 s, `sunset` rises over the build."""
    drop = smoothstep(9.9, 10.15, seconds) * (1.0 - smoothstep(19.6, 21.5, seconds))
    sunset = smoothstep(7.8, 10.0, seconds)
    return TakeReading(drop, sunset)
