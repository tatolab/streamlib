/**
 * Test Pattern Generator - WebGPU Compute Shaders
 *
 * GPU-native test pattern generation for streamlib.
 * Generates patterns directly on GPU, eliminating CPU→GPU transfers.
 *
 * Patterns:
 * - SMPTE bars (mode 0)
 * - Horizontal gradient (mode 1)
 * - Solid color (mode 2)
 * - Checkerboard (mode 3)
 */

struct PatternParams {
    width: u32,
    height: u32,
    mode: u32,        // 0=smpte, 1=gradient, 2=solid, 3=checkerboard
    color_r: u32,     // For solid mode (0-255)
    color_g: u32,
    color_b: u32,
}

@group(0) @binding(0) var<storage, read_write> output_buffer: array<u32>;
@group(0) @binding(1) var<uniform> params: PatternParams;

// Pack RGB into u32 (RGBA format: 0xAABBGGRR)
fn pack_rgba(r: u32, g: u32, b: u32) -> u32 {
    return (255u << 24u) | (b << 16u) | (g << 8u) | r;
}

// SMPTE Color Bars - Classic 7-bar pattern
fn generate_smpte_bars(x: u32, y: u32) -> u32 {
    let bar_width = params.width / 7u;
    let bar = x / bar_width;

    // SMPTE colors: White, Yellow, Cyan, Green, Magenta, Red, Blue
    switch bar {
        case 0u: { return pack_rgba(255u, 255u, 255u); }  // White
        case 1u: { return pack_rgba(255u, 255u, 0u); }    // Yellow
        case 2u: { return pack_rgba(0u, 255u, 255u); }    // Cyan
        case 3u: { return pack_rgba(0u, 255u, 0u); }      // Green
        case 4u: { return pack_rgba(255u, 0u, 255u); }    // Magenta
        case 5u: { return pack_rgba(255u, 0u, 0u); }      // Red
        default: { return pack_rgba(0u, 0u, 255u); }      // Blue
    }
}

// Horizontal Gradient - Black to white
fn generate_gradient(x: u32, y: u32) -> u32 {
    let intensity = (x * 255u) / params.width;
    return pack_rgba(intensity, intensity, intensity);
}

// Solid Color
fn generate_solid(x: u32, y: u32) -> u32 {
    return pack_rgba(params.color_r, params.color_g, params.color_b);
}

// Checkerboard - 8x8 pattern
fn generate_checkerboard(x: u32, y: u32) -> u32 {
    let square_size = min(params.width, params.height) / 8u;
    let square_x = x / square_size;
    let square_y = y / square_size;

    if ((square_x + square_y) % 2u) == 0u {
        return pack_rgba(255u, 255u, 255u);  // White
    } else {
        return pack_rgba(0u, 0u, 0u);        // Black
    }
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    // Bounds check
    if (x >= params.width || y >= params.height) {
        return;
    }

    let pixel_index = y * params.width + x;

    // Generate pattern based on mode
    var color: u32;
    switch params.mode {
        case 0u: { color = generate_smpte_bars(x, y); }
        case 1u: { color = generate_gradient(x, y); }
        case 2u: { color = generate_solid(x, y); }
        case 3u: { color = generate_checkerboard(x, y); }
        default: { color = pack_rgba(255u, 0u, 255u); }  // Error: magenta
    }

    output_buffer[pixel_index] = color;
}
