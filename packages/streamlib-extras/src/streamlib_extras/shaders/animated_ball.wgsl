// Animated Bouncing Ball with Color Cycling Background
//
// Generates a bouncing ball with color cycling on a gradient background.
// All animation is computed on GPU using time uniforms.

struct Uniforms {
    width: u32,
    height: u32,
    time: f32,          // Total elapsed time in seconds
    ball_x: f32,        // Ball position X (pixels)
    ball_y: f32,        // Ball position Y (pixels)
    ball_hue: f32,      // Ball color hue (0-360)
    bg_hue: f32,        // Background hue (0-360)
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;

// HSV to RGB conversion
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let h_prime = (h % 360.0) / 60.0;
    let x = c * (1.0 - abs(h_prime % 2.0 - 1.0));
    let m = v - c;

    var rgb: vec3<f32>;
    if (h_prime < 1.0) {
        rgb = vec3<f32>(c, x, 0.0);
    } else if (h_prime < 2.0) {
        rgb = vec3<f32>(x, c, 0.0);
    } else if (h_prime < 3.0) {
        rgb = vec3<f32>(0.0, c, x);
    } else if (h_prime < 4.0) {
        rgb = vec3<f32>(0.0, x, c);
    } else if (h_prime < 5.0) {
        rgb = vec3<f32>(x, 0.0, c);
    } else {
        rgb = vec3<f32>(c, 0.0, x);
    }

    return rgb + vec3<f32>(m, m, m);
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

    // Color cycling gradient background
    let fx = f32(x) / f32(uniforms.width);
    let fy = f32(y) / f32(uniforms.height);
    let bg_hue_local = uniforms.bg_hue + fx * 30.0;  // Gradient sweep
    let bg_rgb = hsv_to_rgb(bg_hue_local, 0.3, 0.5);

    // Check if pixel is inside ball
    let dx = f32(x) - uniforms.ball_x;
    let dy = f32(y) - uniforms.ball_y;
    let dist = sqrt(dx * dx + dy * dy);
    let ball_radius = 40.0;

    var final_rgb: vec3<f32>;

    if (dist < ball_radius) {
        // Ball color with smooth edge
        let edge_softness = 2.0;
        let edge_factor = smoothstep(ball_radius - edge_softness, ball_radius, dist);
        let ball_rgb = hsv_to_rgb(uniforms.ball_hue, 1.0, 1.0);
        final_rgb = mix(ball_rgb, bg_rgb, edge_factor);
    } else {
        final_rgb = bg_rgb;
    }

    // Pack and write
    output[idx] = pack_rgba(final_rgb.r, final_rgb.g, final_rgb.b, 1.0);
}
