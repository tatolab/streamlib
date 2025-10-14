/**
 * Gaussian Blur - WebGPU Compute Shader (Separable)
 *
 * Implements separable Gaussian blur for streamlib GPU-accelerated effects.
 * Uses 2-pass approach (horizontal + vertical) for O(2k) instead of O(k²) complexity.
 *
 * Performance: For kernel=51, separable is ~4x faster than naive 2D convolution.
 */

struct BlurParams {
    width: u32,
    height: u32,
    kernel_size: u32,
    sigma: f32,
}

@group(0) @binding(0) var<storage, read> input_buffer: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_buffer: array<u32>;
@group(0) @binding(2) var<uniform> params: BlurParams;

// Unpack RGBA from u32 (format: 0xAABBGGRR)
fn unpack_rgba(packed: u32) -> vec4<f32> {
    let r = f32(packed & 0xFFu);
    let g = f32((packed >> 8u) & 0xFFu);
    let b = f32((packed >> 16u) & 0xFFu);
    let a = f32((packed >> 24u) & 0xFFu);
    return vec4<f32>(r, g, b, a) / 255.0;
}

// Pack RGBA to u32
fn pack_rgba(color: vec4<f32>) -> u32 {
    let r = u32(clamp(color.r * 255.0, 0.0, 255.0));
    let g = u32(clamp(color.g * 255.0, 0.0, 255.0));
    let b = u32(clamp(color.b * 255.0, 0.0, 255.0));
    let a = u32(clamp(color.a * 255.0, 0.0, 255.0));
    return r | (g << 8u) | (b << 16u) | (a << 24u);
}

// Compute Gaussian weight
fn gaussian_weight(x: f32, sigma: f32) -> f32 {
    let sigma_sq = sigma * sigma;
    return exp(-(x * x) / (2.0 * sigma_sq));
}

@compute @workgroup_size(16, 16)
fn blur_horizontal(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    // Bounds check
    if (x >= params.width || y >= params.height) {
        return;
    }

    let kernel_radius = i32(params.kernel_size) / 2;
    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;

    // Horizontal blur (sample along x-axis)
    for (var i = -kernel_radius; i <= kernel_radius; i = i + 1) {
        let sample_x = i32(x) + i;

        // Clamp to image bounds
        let clamped_x = clamp(sample_x, 0, i32(params.width) - 1);
        let pixel_index = y * params.width + u32(clamped_x);

        let color = unpack_rgba(input_buffer[pixel_index]);
        let weight = gaussian_weight(f32(i), params.sigma);

        sum += color * weight;
        weight_sum += weight;
    }

    // Normalize and write result
    let result = sum / weight_sum;
    let output_index = y * params.width + x;
    output_buffer[output_index] = pack_rgba(result);
}

@compute @workgroup_size(16, 16)
fn blur_vertical(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    // Bounds check
    if (x >= params.width || y >= params.height) {
        return;
    }

    let kernel_radius = i32(params.kernel_size) / 2;
    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;

    // Vertical blur (sample along y-axis)
    for (var i = -kernel_radius; i <= kernel_radius; i = i + 1) {
        let sample_y = i32(y) + i;

        // Clamp to image bounds
        let clamped_y = clamp(sample_y, 0, i32(params.height) - 1);
        let pixel_index = u32(clamped_y) * params.width + x;

        let color = unpack_rgba(input_buffer[pixel_index]);
        let weight = gaussian_weight(f32(i), params.sigma);

        sum += color * weight;
        weight_sum += weight;
    }

    // Normalize and write result
    let result = sum / weight_sum;
    let output_index = y * params.width + x;
    output_buffer[output_index] = pack_rgba(result);
}
