"""
Blending shaders for WebGPU.

These shaders implement various blending modes for compositing two textures.
These use a different binding layout:
- Binding 0: Base texture (texture_2d<f32>)
- Binding 1: Overlay texture (texture_2d<f32>)
- Binding 2: Output texture (texture_storage_2d<rgba8unorm, write>)
"""

# Alpha blend (normal blend mode)
ALPHA_BLEND_SHADER = """
@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(base_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let base = textureLoad(base_texture, coord, 0);
    let overlay = textureLoad(overlay_texture, coord, 0);

    // Standard alpha blending: result = overlay * alpha + base * (1 - alpha)
    let alpha = overlay.a;
    let result = vec4<f32>(
        mix(base.rgb, overlay.rgb, alpha),
        max(base.a, overlay.a)  // Combined alpha
    );

    textureStore(output_texture, coord, result);
}
"""

# Additive blend (screen mode)
ADDITIVE_BLEND_SHADER = """
@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(base_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let base = textureLoad(base_texture, coord, 0);
    let overlay = textureLoad(overlay_texture, coord, 0);

    // Additive blending: result = base + overlay * alpha
    let result = vec4<f32>(
        clamp(base.rgb + overlay.rgb * overlay.a, vec3<f32>(0.0), vec3<f32>(1.0)),
        max(base.a, overlay.a)
    );

    textureStore(output_texture, coord, result);
}
"""

# Multiply blend
MULTIPLY_BLEND_SHADER = """
@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(base_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let base = textureLoad(base_texture, coord, 0);
    let overlay = textureLoad(overlay_texture, coord, 0);

    // Multiply blending: result = base * overlay
    let blended = base.rgb * overlay.rgb;
    let result = vec4<f32>(
        mix(base.rgb, blended, overlay.a),
        max(base.a, overlay.a)
    );

    textureStore(output_texture, coord, result);
}
"""

# Screen blend (inverse multiply)
SCREEN_BLEND_SHADER = """
@group(0) @binding(0) var base_texture: texture_2d<f32>;
@group(0) @binding(1) var overlay_texture: texture_2d<f32>;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(base_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let base = textureLoad(base_texture, coord, 0);
    let overlay = textureLoad(overlay_texture, coord, 0);

    // Screen blending: result = 1 - (1 - base) * (1 - overlay)
    let blended = vec3<f32>(1.0) - (vec3<f32>(1.0) - base.rgb) * (vec3<f32>(1.0) - overlay.rgb);
    let result = vec4<f32>(
        mix(base.rgb, blended, overlay.a),
        max(base.a, overlay.a)
    );

    textureStore(output_texture, coord, result);
}
"""