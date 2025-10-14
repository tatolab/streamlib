// Animated Waveform Visualization
//
// Composites an animated sine wave onto input frame.
// All rendering done on GPU.

struct Uniforms {
    width: u32,
    height: u32,
    time: f32,          // Total elapsed time in seconds
    wave_offset: f32,   // Wave animation offset (pixels)
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<storage, read> input: array<u32>;
@group(0) @binding(2) var<storage, read_write> output: array<u32>;

// Unpack u32 to RGBA
fn unpack_rgba(packed: u32) -> vec4<f32> {
    let r = f32(packed & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = f32((packed >> 24u) & 0xFFu) / 255.0;
    return vec4<f32>(r, g, b, a);
}

// Pack RGBA into u32
fn pack_rgba(r: f32, g: f32, b: f32, a: f32) -> u32 {
    let r_u8 = u32(clamp(r * 255.0, 0.0, 255.0));
    let g_u8 = u32(clamp(g * 255.0, 0.0, 255.0));
    let b_u8 = u32(clamp(b * 255.0, 0.0, 255.0));
    let a_u8 = u32(clamp(a * 255.0, 0.0, 255.0));
    return r_u8 | (g_u8 << 8u) | (b_u8 << 16u) | (a_u8 << 24u);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    if (x >= uniforms.width || y >= uniforms.height) {
        return;
    }

    let idx = y * uniforms.width + x;

    // Read input pixel
    var color = unpack_rgba(input[idx]);

    // Calculate sine wave position
    let fx = f32(x);
    let wave_y = f32(uniforms.height) / 2.0 + 50.0 * sin((fx + uniforms.wave_offset) * 0.02);

    // Wave thickness (3 pixels)
    let wave_thickness = 3.0;
    let dist_to_wave = abs(f32(y) - wave_y);

    if (dist_to_wave < wave_thickness) {
        // Draw cyan wave with smooth anti-aliasing
        let wave_color = vec3<f32>(0.0, 1.0, 1.0);
        let alpha = smoothstep(wave_thickness, wave_thickness - 1.0, dist_to_wave);
        color = vec4<f32>(
            mix(color.rgb, wave_color, alpha),
            1.0
        );
    }

    // Write output
    output[idx] = pack_rgba(color.r, color.g, color.b, color.a);
}
