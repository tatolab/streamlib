/**
 * WebGPU Compositor Shader
 *
 * Alpha blends two video streams on GPU.
 * Base layer (input_0) with overlay layer (input_1).
 *
 * Format: RGBA packed as u32 (r | (g << 8) | (b << 16) | (a << 24))
 */

struct CompositorParams {
    width: u32,
    height: u32,
    alpha: f32,      // Overlay alpha (0.0 = transparent, 1.0 = opaque)
    mode: u32,       // 0 = alpha_blend, 1 = additive, 2 = multiply
}

@group(0) @binding(0) var<storage, read> input_base: array<u32>;    // Base layer
@group(0) @binding(1) var<storage, read> input_overlay: array<u32>; // Overlay layer
@group(0) @binding(2) var<storage, read_write> output: array<u32>;  // Composited result
@group(0) @binding(3) var<uniform> params: CompositorParams;

// Unpack u32 RGBA to f32 components
fn unpack_rgba(packed: u32) -> vec4<f32> {
    let r = f32(packed & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = f32((packed >> 24u) & 0xFFu) / 255.0;
    return vec4<f32>(r, g, b, a);
}

// Pack f32 RGBA components to u32
fn pack_rgba(color: vec4<f32>) -> u32 {
    let r = u32(clamp(color.r, 0.0, 1.0) * 255.0);
    let g = u32(clamp(color.g, 0.0, 1.0) * 255.0);
    let b = u32(clamp(color.b, 0.0, 1.0) * 255.0);
    let a = u32(clamp(color.a, 0.0, 1.0) * 255.0);
    return r | (g << 8u) | (b << 16u) | (a << 24u);
}

// Alpha blend: base * (1 - alpha) + overlay * alpha
fn alpha_blend(base: vec4<f32>, overlay: vec4<f32>, alpha: f32) -> vec4<f32> {
    // Use overlay's alpha channel if present, otherwise use params alpha
    let overlay_alpha = mix(alpha, overlay.a, overlay.a);
    return base * (1.0 - overlay_alpha) + overlay * overlay_alpha;
}

// Additive blend: base + overlay * alpha
fn additive_blend(base: vec4<f32>, overlay: vec4<f32>, alpha: f32) -> vec4<f32> {
    return clamp(base + overlay * alpha, vec4<f32>(0.0), vec4<f32>(1.0));
}

// Multiply blend: base * overlay
fn multiply_blend(base: vec4<f32>, overlay: vec4<f32>) -> vec4<f32> {
    return base * overlay;
}

@compute @workgroup_size(16, 16)
fn composite(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    // Bounds check
    if (x >= params.width || y >= params.height) {
        return;
    }

    let idx = y * params.width + x;

    // Unpack pixels
    let base = unpack_rgba(input_base[idx]);
    let overlay = unpack_rgba(input_overlay[idx]);

    // Composite based on mode
    var result: vec4<f32>;
    switch (params.mode) {
        case 0u: { // Alpha blend
            result = alpha_blend(base, overlay, params.alpha);
        }
        case 1u: { // Additive
            result = additive_blend(base, overlay, params.alpha);
        }
        case 2u: { // Multiply
            result = multiply_blend(base, overlay);
        }
        default: { // Default to alpha blend
            result = alpha_blend(base, overlay, params.alpha);
        }
    }

    // Pack and write
    output[idx] = pack_rgba(result);
}
