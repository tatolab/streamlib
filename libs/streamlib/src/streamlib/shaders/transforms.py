"""
Transformation shaders for WebGPU.

These shaders implement geometric transformations like flips, rotations, and scaling.
All shaders follow the streamlib convention:
- Binding 0: Input texture (texture_2d<f32>)
- Binding 1: Output texture (texture_storage_2d<rgba8unorm, write>)
"""

# Flip horizontally
FLIP_HORIZONTAL_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Flip X coordinate
    let flipped_coord = vec2<i32>(i32(dims.x) - coord.x - 1, coord.y);
    let color = textureLoad(input_texture, flipped_coord, 0);

    textureStore(output_texture, coord, color);
}
"""

# Flip vertically
FLIP_VERTICAL_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Flip Y coordinate
    let flipped_coord = vec2<i32>(coord.x, i32(dims.y) - coord.y - 1);
    let color = textureLoad(input_texture, flipped_coord, 0);

    textureStore(output_texture, coord, color);
}
"""

# Rotate 90 degrees clockwise
ROTATE_90_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let in_dims = textureDimensions(input_texture);
    let out_dims = textureDimensions(output_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(out_dims.x) || coord.y >= i32(out_dims.y)) {
        return;
    }

    // Rotate 90 degrees: new_x = old_y, new_y = width - old_x - 1
    let source_coord = vec2<i32>(coord.y, i32(in_dims.x) - coord.x - 1);

    // Ensure source coordinate is in bounds
    if (source_coord.x >= 0 && source_coord.x < i32(in_dims.x) &&
        source_coord.y >= 0 && source_coord.y < i32(in_dims.y)) {
        let color = textureLoad(input_texture, source_coord, 0);
        textureStore(output_texture, coord, color);
    }
}
"""

# Scale with bilinear interpolation
SCALE_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const SCALE_X: f32 = 2.0;  // Horizontal scale factor
const SCALE_Y: f32 = 2.0;  // Vertical scale factor

// Bilinear interpolation
fn bilinear_sample(tex: texture_2d<f32>, uv: vec2<f32>, dims: vec2<u32>) -> vec4<f32> {
    let pixel_coord = uv * vec2<f32>(dims) - vec2<f32>(0.5);
    let floor_coord = floor(pixel_coord);
    let fract_coord = fract(pixel_coord);

    let x0 = i32(clamp(floor_coord.x, 0.0, f32(dims.x) - 1.0));
    let y0 = i32(clamp(floor_coord.y, 0.0, f32(dims.y) - 1.0));
    let x1 = i32(clamp(floor_coord.x + 1.0, 0.0, f32(dims.x) - 1.0));
    let y1 = i32(clamp(floor_coord.y + 1.0, 0.0, f32(dims.y) - 1.0));

    let tl = textureLoad(tex, vec2<i32>(x0, y0), 0);
    let tr = textureLoad(tex, vec2<i32>(x1, y0), 0);
    let bl = textureLoad(tex, vec2<i32>(x0, y1), 0);
    let br = textureLoad(tex, vec2<i32>(x1, y1), 0);

    let top = mix(tl, tr, fract_coord.x);
    let bottom = mix(bl, br, fract_coord.x);

    return mix(top, bottom, fract_coord.y);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_dims = textureDimensions(output_texture);
    let in_dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(out_dims.x) || coord.y >= i32(out_dims.y)) {
        return;
    }

    // Calculate UV coordinates in output space
    let uv_out = vec2<f32>(f32(coord.x), f32(coord.y)) / vec2<f32>(out_dims);

    // Transform to input space
    let uv_in = uv_out * vec2<f32>(SCALE_X, SCALE_Y);

    // Sample with bilinear interpolation
    let color = bilinear_sample(input_texture, uv_in, in_dims);

    textureStore(output_texture, coord, color);
}
"""