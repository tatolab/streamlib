# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Pixels on the GPU: the effect the stream wires after its source.

Importable as `nodes.halftone_lens_effect:HalftoneLensEffect`, which is the
name the engine spawns this node's child interpreter with.
"""

from tatolab.stream import (
    GlslPixelEffect,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    msgpack,
    node,
)

# The whole effect: the output pixel for the source pixel at `at`, with each
# channel in 0.0-1.0. A lens drifts around the frame and turns what it covers
# into dots. `dials` are the values `process()` hands it each frame. Edit it
# while `tatolab dev` runs to see the change.
HALFTONE_LENS_GLSL = """
// How big the drifting lens is, as a share of the frame's height.
const float LENS_RADIUS = 0.3;
// How many pixels apart the dots sit under the lens.
const float DOT_CELL_SIZE = 8.0;
// How far the gaps between the dots fade toward grey.
const float GAP_FADE = 0.35;

vec4 effect(vec4 source, ivec2 at) {
    vec2 pixel = vec2(at) + 0.5;
    vec2 lens_centre = dials.frame_size * vec2(
        0.5 + 0.3 * sin(dials.seconds * 0.4),
        0.5 + 0.25 * sin(dials.seconds * 0.55)
    );
    float distance_from_lens_centre = length(pixel - lens_centre) / dials.frame_size.y;
    float under_the_lens =
        1.0 - smoothstep(LENS_RADIUS * 0.92, LENS_RADIUS, distance_from_lens_centre);

    // Each cell keeps a round dot of the picture and fades the gap around it
    // toward grey, so the dots show on every colour, black included.
    vec2 cell_centre = (floor(pixel / DOT_CELL_SIZE) + 0.5) * DOT_CELL_SIZE;
    float dot_radius = DOT_CELL_SIZE * 0.42;
    float in_the_dot = 1.0 - smoothstep(dot_radius - 1.0, dot_radius, length(pixel - cell_centre));
    vec3 gap = mix(source.rgb, vec3(0.5), GAP_FADE);
    vec3 dotted = mix(gap, source.rgb, in_the_dot);

    return vec4(mix(source.rgb, dotted, under_the_lens), source.a);
}
"""

NANOSECONDS_PER_SECOND = 1_000_000_000


@node
class HalftoneLensEffect:
    """Turns what a slowly drifting lens covers into dots on the GPU, and passes each frame on."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.halftone_lens_pixel_effect = GlslPixelEffect.compile(
            ctx.gpu_full_access,
            effect_glsl=HALFTONE_LENS_GLSL,
            dials={"seconds": "float", "frame_size": "vec2"},
        )
        self.first_frame_at_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        # `ctx.time` is the monotonic clock every node shares, in ns.
        if self.first_frame_at_ns is None:
            self.first_frame_at_ns = ctx.time
        halftone_frame_record = self.halftone_lens_pixel_effect.apply_to_frame(
            ctx.gpu_limited_access,
            frame,
            dials={
                "seconds": (ctx.time - self.first_frame_at_ns) / NANOSECONDS_PER_SECOND,
                "frame_size": (frame.width, frame.height),
            },
        )
        ctx.outputs.write("video_to_downstream", msgpack.pack(halftone_frame_record))
