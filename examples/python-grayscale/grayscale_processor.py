# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Grayscale video processor using GPU shader.

This processor converts RGB video frames to grayscale using a WGSL
compute shader executed on the GPU.
"""

from streamlib import processor, input_port, output_port

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

    @input_port(frame_type="VideoFrame", description="RGB video input")
    def video_in(self):
        pass

    @output_port(frame_type="VideoFrame", description="Grayscale video output")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Compile the grayscale shader on startup."""
        self.shader = ctx.gpu.compile_shader("grayscale", GRAYSCALE_SHADER)
        print(f"GrayscaleProcessor: Shader compiled successfully")

    def process(self, ctx):
        """Process each frame through the grayscale shader."""
        frame = ctx.inputs.video_in.read()
        if frame is None:
            return

        # Dispatch shader to convert frame to grayscale
        output_texture = ctx.gpu.dispatch(
            self.shader,
            {"input_texture": frame.texture},
            frame.width,
            frame.height,
        )

        # Write output frame with new texture, preserving metadata
        ctx.outputs.video_out.write(frame.with_texture(output_texture))

    def teardown(self, ctx):
        """Cleanup on shutdown."""
        print(f"GrayscaleProcessor: Teardown complete")
