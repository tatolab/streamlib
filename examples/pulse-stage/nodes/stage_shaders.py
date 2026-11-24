# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The stage's pictures, each a Slang shader the runtime compiles and runs on the GPU.

`OrbitLights` draws seven coloured lights circling on black, swelling on the beat.
`Kaleidoscope` folds whatever it is given into mirrored wedges around the centre, so the
lights, which never repeat on their own, come out symmetrical: a frame shows at a glance
whether the shader ran. `ClockDigits` burns the stream's clock into the corner as seven-segment
digits, so a still frame also shows that the stream is moving.

Each helper is named for its node, since the runtime fuses neighbouring shaders into one
kernel. Every picture is 16:9, so a point is measured in frame heights from the centre.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import Color, Description, Texture, NumberRange, NumberStep, node

ORBIT_LIGHTS_SLANG = """
static const float OrbitLights_WIDTH_IN_HEIGHTS = 1.7777778;
static const int OrbitLights_LIGHT_COUNT = 7;

float3 OrbitLights_hue(float turn)
{
    return 0.5 + 0.5 * cos(6.2831853 * (turn + float3(0.0, 0.33, 0.67)));
}

float4 OrbitLights(ImageInput size_from, float seconds, float speed, float pulse)
{
    float2 point = (size_from.uv - 0.5) * float2(OrbitLights_WIDTH_IN_HEIGHTS, 1.0);
    float3 light = float3(0.01, 0.01, 0.03);
    for (int index = 0; index < OrbitLights_LIGHT_COUNT; index++)
    {
        float lane = float(index) / float(OrbitLights_LIGHT_COUNT);
        float orbit = 0.1 + 0.045 * float(index);
        float angle = seconds * speed * (0.55 + 0.2 * float(index)) + lane * 6.2831853;
        float2 centre = float2(cos(angle) * orbit * 1.7, sin(angle * 1.3) * orbit);
        float reach = 0.022 + 0.018 * pulse;
        float distance_squared = dot(point - centre, point - centre);
        light += OrbitLights_hue(lane + seconds * 0.04) * (reach * reach) / (distance_squared + 0.0003);
    }
    return float4(saturate(light), 1.0);
}
"""

KALEIDOSCOPE_SLANG = """
static const float Kaleidoscope_WIDTH_IN_HEIGHTS = 1.7777778;
static const float Kaleidoscope_TURN = 6.2831853;

float Kaleidoscope_ring(float radius, float pulse)
{
    float ring_radius = 0.12 + 0.4 * (1.0 - pulse);
    float off_the_ring = (radius - ring_radius) * 36.0;
    return exp(-off_the_ring * off_the_ring) * pulse;
}

float4 Kaleidoscope(ImageInput image, float segments, float spin, float pulse)
{
    float2 centred = (image.uv - 0.5) * float2(Kaleidoscope_WIDTH_IN_HEIGHTS, 1.0);
    float radius = length(centred);
    float wedge = Kaleidoscope_TURN / max(floor(segments), 2.0);
    float angle = atan2(centred.y, centred.x) + spin * Kaleidoscope_TURN;
    float within_the_wedge = angle - wedge * floor(angle / wedge);
    float mirrored = abs(within_the_wedge - 0.5 * wedge);
    float zoom = 1.0 - 0.2 * pulse;
    float2 folded = float2(cos(mirrored), sin(mirrored)) * radius * zoom;
    float2 source = saturate(folded / float2(Kaleidoscope_WIDTH_IN_HEIGHTS, 1.0) + 0.5);
    float3 colour = image.sample(source).rgb * (0.8 + 0.7 * pulse);
    colour += Kaleidoscope_ring(radius, pulse) * float3(1.0, 0.85, 0.6);
    return float4(saturate(colour), 1.0);
}
"""

CLOCK_DIGITS_SLANG = """
static const float ClockDigits_WIDTH_IN_HEIGHTS = 1.7777778;
static const uint ClockDigits_LIT_SEGMENTS[10] = { 0x3Fu, 0x06u, 0x5Bu, 0x4Fu, 0x66u, 0x6Du, 0x7Du, 0x07u, 0x7Fu, 0x6Fu };
static const int ClockDigits_CHARACTER_COUNT = 7;
static const int ClockDigits_COLON = 2;
static const int ClockDigits_POINT = 5;

float ClockDigits_bar(float2 point, float2 from, float2 to)
{
    float2 along = to - from;
    float share = saturate(dot(point - from, along) / dot(along, along));
    float distance_to_the_bar = length(point - from - along * share);
    return 1.0 - smoothstep(0.06, 0.1, distance_to_the_bar);
}

float ClockDigits_dot(float2 point, float2 centre)
{
    return 1.0 - smoothstep(0.07, 0.11, length((point - centre) * float2(0.6, 1.0)));
}

// How much of `point`, in a digit's cell from (0, 0) at its top left to (1, 1), the digit covers.
float ClockDigits_digit(float2 point, uint digit)
{
    uint lit = ClockDigits_LIT_SEGMENTS[min(digit, 9u)];
    float2 top_left = float2(0.15, 0.08);
    float2 top_right = float2(0.85, 0.08);
    float2 middle_left = float2(0.15, 0.5);
    float2 middle_right = float2(0.85, 0.5);
    float2 bottom_left = float2(0.15, 0.92);
    float2 bottom_right = float2(0.85, 0.92);
    float cover = 0.0;
    if ((lit & 0x01u) != 0u) cover = max(cover, ClockDigits_bar(point, top_left, top_right));
    if ((lit & 0x02u) != 0u) cover = max(cover, ClockDigits_bar(point, top_right, middle_right));
    if ((lit & 0x04u) != 0u) cover = max(cover, ClockDigits_bar(point, middle_right, bottom_right));
    if ((lit & 0x08u) != 0u) cover = max(cover, ClockDigits_bar(point, bottom_left, bottom_right));
    if ((lit & 0x10u) != 0u) cover = max(cover, ClockDigits_bar(point, middle_left, bottom_left));
    if ((lit & 0x20u) != 0u) cover = max(cover, ClockDigits_bar(point, top_left, middle_left));
    if ((lit & 0x40u) != 0u) cover = max(cover, ClockDigits_bar(point, middle_left, middle_right));
    return cover;
}

// The digit the clock shows in character `index` of `mm:ss.t` at `tenths` of a second.
uint ClockDigits_digit_at(int index, uint tenths)
{
    uint seconds = tenths / 10u;
    uint minutes = (seconds / 60u) % 100u;
    seconds = seconds % 60u;
    if (index == 0) return minutes / 10u;
    if (index == 1) return minutes % 10u;
    if (index == 3) return seconds / 10u;
    if (index == 4) return seconds % 10u;
    return tenths % 10u;
}

float4 ClockDigits(ImageInput image, float seconds, float4 colour, float size)
{
    float height = 0.09 * max(size, 0.1);
    float advance = 0.78 * height;
    float2 origin = float2(0.05, 0.05);
    float2 point = image.uv * float2(ClockDigits_WIDTH_IN_HEIGHTS, 1.0);
    uint tenths = uint(max(seconds, 0.0) * 10.0);

    float2 plate_from = origin - 0.25 * height;
    float2 plate_to = origin + float2(advance * float(ClockDigits_CHARACTER_COUNT), height) + 0.25 * height;
    bool on_the_plate = all(point >= plate_from) && all(point <= plate_to);

    float cover = 0.0;
    for (int index = 0; index < ClockDigits_CHARACTER_COUNT; index++)
    {
        float2 cell = (point - origin - float2(advance * float(index), 0.0)) / float2(0.62 * height, height);
        if (any(cell < 0.0) || any(cell > 1.0))
            continue;
        if (index == ClockDigits_COLON)
            cover = max(cover, max(ClockDigits_dot(cell, float2(0.5, 0.3)), ClockDigits_dot(cell, float2(0.5, 0.7))));
        else if (index == ClockDigits_POINT)
            cover = max(cover, ClockDigits_dot(cell, float2(0.5, 0.92)));
        else
            cover = max(cover, ClockDigits_digit(cell, ClockDigits_digit_at(index, tenths)));
    }

    float3 behind = on_the_plate ? image.pixel.rgb * 0.35 : image.pixel.rgb;
    return float4(lerp(behind, colour.rgb, cover * colour.a), image.pixel.a);
}
"""


#: The clock's colour unless set: a warm white that reads over every hue the lights take.
WARM_WHITE = Color(1.0, 0.92, 0.55, 1.0)


@node.meta(
    category="Pulse Stage",
    description="Seven coloured lights circling on black. Wire a clock into seconds to set them moving and a pulse to make them swell on the beat.",
)
@node.shader(ORBIT_LIGHTS_SLANG)
def OrbitLights(
    size_from: Annotated[
        Texture, Description("Any picture: the lights are drawn at its size.")
    ],
    seconds: Annotated[float, Description("The stream's clock, in seconds.")] = 0.0,
    speed: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=4.0),
        NumberStep(0.1),
        Description("How fast the lights circle."),
    ] = 1.0,
    pulse: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("How far the lights swell out, on the beat."),
    ] = 0.0,
) -> Texture: ...


@node.meta(
    category="Pulse Stage",
    description="Fold the picture into mirrored wedges around its centre, with a ring that bursts outward on each pulse.",
)
@node.shader(KALEIDOSCOPE_SLANG)
def Kaleidoscope(
    image: Annotated[Texture, Description("The picture to fold.")],
    segments: Annotated[
        float,
        NumberRange(minimum=2.0, maximum=16.0),
        NumberStep(1.0),
        Description("How many mirrored wedges."),
    ] = 6.0,
    spin: Annotated[
        float,
        NumberStep(0.01),
        Description("How far the wedges are turned, in whole turns."),
    ] = 0.0,
    pulse: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("How far it zooms in and rings out, on the beat."),
    ] = 0.0,
) -> Texture: ...


@node.meta(
    category="Pulse Stage",
    description="Burn a clock reading mm:ss.t into the top-left corner, so even a still frame shows the stream is moving.",
)
@node.shader(CLOCK_DIGITS_SLANG)
def ClockDigits(
    image: Annotated[Texture, Description("The picture to stamp.")],
    seconds: Annotated[float, Description("The time to show, in seconds.")] = 0.0,
    colour: Annotated[Color, Description("The digits' colour.")] = WARM_WHITE,
    size: Annotated[
        float,
        NumberRange(minimum=0.1, maximum=4.0),
        NumberStep(0.1),
        Description("How tall the digits are; 1 is a tenth of the frame."),
    ] = 1.0,
) -> Texture: ...
