# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Grayscale video processor using GPU shader.

This processor converts RGB video frames to grayscale using a WGSL
compute shader executed on the GPU.
"""

from streamlib import processor, input, output

# WGSL compute shader for grayscale conversion
GRAYSCALE_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    if global_id.x >= dims.x || global_id.y >= dims.y {
        return;
    }

    let color = textureLoad(input_texture, vec2<i32>(global_id.xy), 0);

    // ITU-R BT.709 luma coefficients
    let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));

    textureStore(output_texture, vec2<i32>(global_id.xy), vec4<f32>(gray, gray, gray, color.a));
}
"""


@processor(name="GrayscaleProcessor", description="Convert video to grayscale using GPU shader")
class GrayscaleProcessor:
    """Converts RGB video frames to grayscale.

    Uses ITU-R BT.709 luma coefficients for accurate grayscale conversion.
    All processing happens on the GPU via a WGSL compute shader.
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Compile the grayscale shader on startup."""
        self.shader = ctx.gpu.compile_shader("grayscale", GRAYSCALE_SHADER)
        print(f"GrayscaleProcessor: Shader compiled successfully")

    def process(self, ctx):
        """Process each frame through the grayscale shader."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        # Get specific fields
        texture = ctx.input("video_in").get("texture")
        width = frame["width"]
        height = frame["height"]

        # Dispatch shader to convert frame to grayscale
        output_texture = ctx.gpu.dispatch(
            self.shader,
            {"input_texture": texture},
            width,
            height,
        )

        # Write output frame with new texture
        ctx.output("video_out").set({
            "texture": output_texture,
            "width": width,
            "height": height,
            "timestamp_ns": frame["timestamp_ns"],
            "frame_number": frame["frame_number"],
        })

    def teardown(self, ctx):
        """Cleanup on shutdown."""
        print(f"GrayscaleProcessor: Teardown complete")
