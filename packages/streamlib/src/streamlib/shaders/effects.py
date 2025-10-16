"""
Special effect shaders for WebGPU.

These shaders implement various visual effects and artistic filters.
All shaders follow the streamlib convention:
- Binding 0: Input texture (texture_2d<f32>)
- Binding 1: Output texture (texture_storage_2d<rgba8unorm, write>)
"""

# Edge detection using Sobel operator
EDGE_DETECT_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Sobel X kernel: [-1, 0, 1; -2, 0, 2; -1, 0, 1]
    // Sobel Y kernel: [-1, -2, -1; 0, 0, 0; 1, 2, 1]

    var gx = vec3<f32>(0.0);
    var gy = vec3<f32>(0.0);

    // Apply Sobel kernels
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sample_coord = coord + vec2<i32>(dx, dy);
            if (sample_coord.x >= 0 && sample_coord.x < i32(dims.x) &&
                sample_coord.y >= 0 && sample_coord.y < i32(dims.y)) {
                let pixel = textureLoad(input_texture, sample_coord, 0).rgb;

                // Sobel X
                gx += pixel * f32(dx) * f32(2 - abs(dy));

                // Sobel Y
                gy += pixel * f32(dy) * f32(2 - abs(dx));
            }
        }
    }

    // Calculate edge magnitude
    let edge = length(vec2<f32>(length(gx), length(gy)));
    let result = vec4<f32>(edge, edge, edge, 1.0);

    textureStore(output_texture, coord, result);
}
"""

# Sharpen filter
SHARPEN_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Sharpen kernel: [0, -1, 0; -1, 5, -1; 0, -1, 0]
    var color = textureLoad(input_texture, coord, 0) * 5.0;

    // Subtract neighboring pixels
    color -= textureLoad(input_texture, coord + vec2<i32>(-1, 0), 0);
    color -= textureLoad(input_texture, coord + vec2<i32>(1, 0), 0);
    color -= textureLoad(input_texture, coord + vec2<i32>(0, -1), 0);
    color -= textureLoad(input_texture, coord + vec2<i32>(0, 1), 0);

    // Clamp to valid range
    color = clamp(color, vec4<f32>(0.0), vec4<f32>(1.0));

    textureStore(output_texture, coord, color);
}
"""

# Emboss effect
EMBOSS_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Emboss kernel: [-2, -1, 0; -1, 1, 1; 0, 1, 2]
    var color = vec4<f32>(0.0);

    color -= textureLoad(input_texture, coord + vec2<i32>(-1, -1), 0) * 2.0;
    color -= textureLoad(input_texture, coord + vec2<i32>(0, -1), 0);
    color -= textureLoad(input_texture, coord + vec2<i32>(-1, 0), 0);
    color += textureLoad(input_texture, coord, 0);
    color += textureLoad(input_texture, coord + vec2<i32>(1, 0), 0);
    color += textureLoad(input_texture, coord + vec2<i32>(0, 1), 0);
    color += textureLoad(input_texture, coord + vec2<i32>(1, 1), 0) * 2.0;

    // Add gray offset for emboss effect
    color = color + vec4<f32>(0.5, 0.5, 0.5, 0.0);

    // Clamp and preserve alpha
    let original_alpha = textureLoad(input_texture, coord, 0).a;
    color = clamp(color, vec4<f32>(0.0), vec4<f32>(1.0));
    color.a = original_alpha;

    textureStore(output_texture, coord, color);
}
"""

# Pixelate effect
PIXELATE_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const PIXEL_SIZE: i32 = 8;  // Size of pixelation blocks

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Find the top-left corner of the pixel block
    let block_coord = vec2<i32>(
        (coord.x / PIXEL_SIZE) * PIXEL_SIZE,
        (coord.y / PIXEL_SIZE) * PIXEL_SIZE
    );

    // Sample from the center of the pixel block
    let sample_coord = block_coord + vec2<i32>(PIXEL_SIZE / 2);
    let color = textureLoad(input_texture, sample_coord, 0);

    textureStore(output_texture, coord, color);
}
"""

# Vignette effect (darkening towards edges)
VIGNETTE_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const STRENGTH: f32 = 0.5;  // Vignette strength (0.0 to 1.0)
const RADIUS: f32 = 0.75;   // Inner radius where vignette starts

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Calculate distance from center (normalized to 0-1)
    let center = vec2<f32>(f32(dims.x), f32(dims.y)) * 0.5;
    let pos = vec2<f32>(f32(coord.x), f32(coord.y));
    let dist = length(pos - center) / length(center);

    // Calculate vignette factor
    let vignette = smoothstep(RADIUS, 1.0, dist);
    let factor = 1.0 - vignette * STRENGTH;

    // Apply vignette
    let result = vec4<f32>(color.rgb * factor, color.a);

    textureStore(output_texture, coord, result);
}
"""

# Chromatic aberration (RGB channel separation)
CHROMATIC_ABERRATION_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const OFFSET: f32 = 0.01;  // Aberration amount (0.0 to 0.05)

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Calculate UV coordinates (0-1 range)
    let uv = vec2<f32>(f32(coord.x), f32(coord.y)) / vec2<f32>(f32(dims.x), f32(dims.y));

    // Calculate distance from center
    let center = vec2<f32>(0.5, 0.5);
    let dir = uv - center;
    let dist = length(dir);

    // Calculate aberration offsets
    let aberration = dir * OFFSET * dist;

    // Sample each color channel with different offsets
    let r_coord = vec2<i32>((uv - aberration) * vec2<f32>(f32(dims.x), f32(dims.y)));
    let g_coord = coord;  // Green channel stays centered
    let b_coord = vec2<i32>((uv + aberration) * vec2<f32>(f32(dims.x), f32(dims.y)));

    // Clamp coordinates to texture bounds
    let r = textureLoad(input_texture, clamp(r_coord, vec2<i32>(0), dims - vec2<u32>(1)), 0).r;
    let g = textureLoad(input_texture, coord, 0).g;
    let b = textureLoad(input_texture, clamp(b_coord, vec2<i32>(0), dims - vec2<u32>(1)), 0).b;
    let a = textureLoad(input_texture, coord, 0).a;

    textureStore(output_texture, coord, vec4<f32>(r, g, b, a));
}
"""