// Pulsing Corner Markers and Animated Border
//
// Composites pulsing corner markers and animated border onto input frame.
// All compositing done on GPU.

struct Uniforms {
    width: u32,
    height: u32,
    time: f32,           // Total elapsed time in seconds
    pulse_intensity: f32, // Pulse value 0.0-1.0
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

    // Corner markers (30x30 pixels)
    let marker_size = 30u;
    let is_top_left = x < marker_size && y < marker_size;
    let is_top_right = x >= (uniforms.width - marker_size) && y < marker_size;
    let is_bottom_left = x < marker_size && y >= (uniforms.height - marker_size);
    let is_bottom_right = x >= (uniforms.width - marker_size) && y >= (uniforms.height - marker_size);

    if (is_top_left || is_top_right || is_bottom_left || is_bottom_right) {
        // Pulsing orange/red markers
        let marker_color = vec3<f32>(0.0, uniforms.pulse_intensity, 1.0);
        color = vec4<f32>(marker_color, 1.0);
    }

    // Animated border (5 pixels thick)
    let border_thickness = 5u;
    let is_border = x < border_thickness ||
                   x >= (uniforms.width - border_thickness) ||
                   y < border_thickness ||
                   y >= (uniforms.height - border_thickness);

    if (is_border && !is_top_left && !is_top_right && !is_bottom_left && !is_bottom_right) {
        // Animated border color
        let border_color = vec3<f32>(
            uniforms.pulse_intensity,
            1.0 - uniforms.pulse_intensity,
            0.5
        );
        // Alpha blend border
        let alpha = 0.7;
        color = vec4<f32>(
            mix(color.rgb, border_color, alpha),
            1.0
        );
    }

    // Write output
    output[idx] = pack_rgba(color.r, color.g, color.b, color.a);
}
