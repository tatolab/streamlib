# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Picture nodes that move, turn and repeat a picture."""

from __future__ import annotations

from tatolab.stream import ImageFrame, node


@node.meta(
    category="Transform",
    description="Move, resize and turn a picture. Move is a fraction of the frame, scale 1 is full size, rotate is in turns (0.25 is a quarter turn). Outside the picture is see-through.",
)
@node.shader(
    """
float4 Transform(ImageInput image, float move_x, float move_y, float scale, float rotate)
{
    float2 aspect = float2(1.0, 0.5625);
    float2 from_center = (image.uv - 0.5 - float2(move_x, move_y)) * aspect;
    float turn = -rotate * 6.2831853;
    float2 turned = float2(from_center.x * cos(turn) - from_center.y * sin(turn), from_center.x * sin(turn) + from_center.y * cos(turn));
    float2 source = turned / (aspect * max(scale, 0.0001)) + 0.5;
    float inside = step(0.0, source.x) * step(source.x, 1.0) * step(0.0, source.y) * step(source.y, 1.0);
    return image.sample(source) * inside;
}
""",
)
def Transform(  # noqa: N802
    image: ImageFrame,
    move_x: float = 0.0,
    move_y: float = 0.0,
    scale: float = 1.0,
    rotate: float = 0.0,
) -> ImageFrame: ...


@node.meta(
    category="Transform",
    description="Cut away the edges of a picture, each by a fraction of the frame. What is cut is see-through.",
)
@node.shader(
    """
float4 Crop(ImageInput image, float left, float right, float top, float bottom)
{
    float inside = step(left, image.uv.x) * step(image.uv.x, 1.0 - right) * step(top, image.uv.y) * step(image.uv.y, 1.0 - bottom);
    return image.pixel * inside;
}
""",
)
def Crop(  # noqa: N802
    image: ImageFrame,
    left: float = 0.0,
    right: float = 0.0,
    top: float = 0.0,
    bottom: float = 0.0,
) -> ImageFrame: ...


@node.meta(
    category="Transform",
    description="Mirror a picture left to right, top to bottom, or both. Set each to 1 to flip.",
)
@node.shader(
    """
float4 Flip(ImageInput image, float horizontal, float vertical)
{
    float2 source = float2(horizontal > 0.5 ? 1.0 - image.uv.x : image.uv.x, vertical > 0.5 ? 1.0 - image.uv.y : image.uv.y);
    return image.sample(source);
}
""",
)
def Flip(image: ImageFrame, horizontal: float = 1.0, vertical: float = 0.0) -> ImageFrame: ...  # noqa: N802


@node.meta(
    category="Transform",
    description="Repeat a picture in a grid. Wire a Ramp into scroll to slide the tiles forever.",
)
@node.shader(
    """
float4 Tile(ImageInput image, float across, float down, float scroll_x, float scroll_y)
{
    return image.sample(frac(image.uv * float2(max(across, 0.0001), max(down, 0.0001)) + float2(scroll_x, scroll_y)));
}
""",
)
def Tile(  # noqa: N802
    image: ImageFrame,
    across: float = 2.0,
    down: float = 2.0,
    scroll_x: float = 0.0,
    scroll_y: float = 0.0,
) -> ImageFrame: ...


@node.meta(
    category="Transform",
    description="Reflect one half of a picture onto the other, for a symmetric image.",
)
@node.shader(
    """
float4 Mirror(ImageInput image, float horizontal, float vertical)
{
    float2 source = image.uv;
    if (horizontal > 0.5) source.x = 0.5 - abs(source.x - 0.5);
    if (vertical > 0.5) source.y = 0.5 - abs(source.y - 0.5);
    return image.sample(source);
}
""",
)
def Mirror(image: ImageFrame, horizontal: float = 1.0, vertical: float = 0.0) -> ImageFrame: ...  # noqa: N802


@node.meta(
    category="Transform",
    description="Fold a picture into mirrored wedges around its centre. Wire a Ramp into rotate to make it spin.",
)
@node.shader(
    """
float4 Kaleidoscope(ImageInput image, float segments, float rotate, float zoom)
{
    float2 aspect = float2(1.0, 0.5625);
    float2 from_center = (image.uv - 0.5) * aspect;
    float radius = length(from_center) / max(zoom, 0.0001);
    float slice = 3.14159265 / max(segments, 1.0);
    float angle = atan2(from_center.y, from_center.x) + rotate * 6.2831853;
    angle = abs(fmod(abs(angle), 2.0 * slice) - slice);
    float2 source = float2(cos(angle), sin(angle)) * radius / aspect + 0.5;
    return image.sample(saturate(source));
}
""",
)
def Kaleidoscope(  # noqa: N802
    image: ImageFrame, segments: float = 6.0, rotate: float = 0.0, zoom: float = 1.0
) -> ImageFrame: ...
