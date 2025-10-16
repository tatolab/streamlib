"""
Color filter shaders for WebGPU.

These shaders implement common color adjustments and filters.
All shaders follow the streamlib convention:
- Binding 0: Input texture (texture_2d<f32>)
- Binding 1: Output texture (texture_storage_2d<rgba8unorm, write>)
"""

# Convert to grayscale
GRAYSCALE_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Standard luminance weights (ITU-R BT.709)
    let gray = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let result = vec4<f32>(gray, gray, gray, color.a);

    textureStore(output_texture, coord, result);
}
"""

# Sepia tone effect
SEPIA_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Sepia tone matrix
    var result: vec4<f32>;
    result.r = min(1.0, dot(color.rgb, vec3<f32>(0.393, 0.769, 0.189)));
    result.g = min(1.0, dot(color.rgb, vec3<f32>(0.349, 0.686, 0.168)));
    result.b = min(1.0, dot(color.rgb, vec3<f32>(0.272, 0.534, 0.131)));
    result.a = color.a;

    textureStore(output_texture, coord, result);
}
"""

# Brightness adjustment (-1.0 to 1.0)
BRIGHTNESS_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

// Uniform buffer would be better for parameters, but for simplicity using constant
const BRIGHTNESS: f32 = 0.2;  // Adjust this value (-1.0 to 1.0)

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);
    let adjusted = vec4<f32>(
        clamp(color.r + BRIGHTNESS, 0.0, 1.0),
        clamp(color.g + BRIGHTNESS, 0.0, 1.0),
        clamp(color.b + BRIGHTNESS, 0.0, 1.0),
        color.a
    );

    textureStore(output_texture, coord, adjusted);
}
"""

# Contrast adjustment (0.0 to 2.0, where 1.0 is normal)
CONTRAST_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const CONTRAST: f32 = 1.5;  // Adjust this value (0.0 to 2.0)

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Apply contrast around middle gray (0.5)
    let adjusted = vec4<f32>(
        clamp((color.r - 0.5) * CONTRAST + 0.5, 0.0, 1.0),
        clamp((color.g - 0.5) * CONTRAST + 0.5, 0.0, 1.0),
        clamp((color.b - 0.5) * CONTRAST + 0.5, 0.0, 1.0),
        color.a
    );

    textureStore(output_texture, coord, adjusted);
}
"""

# Saturation adjustment (0.0 = grayscale, 1.0 = normal, 2.0 = double saturation)
SATURATION_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const SATURATION: f32 = 1.5;  // Adjust this value

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Calculate luminance
    let luminance = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));

    // Interpolate between grayscale and original color
    let adjusted = vec4<f32>(
        mix(luminance, color.r, SATURATION),
        mix(luminance, color.g, SATURATION),
        mix(luminance, color.b, SATURATION),
        color.a
    );

    textureStore(output_texture, coord, adjusted);
}
"""

# Hue shift (0.0 to 1.0, where 0.0 and 1.0 are the original hue)
HUE_SHIFT_SHADER = """
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

const HUE_SHIFT: f32 = 0.1;  // Shift amount (0.0 to 1.0)

// RGB to HSV conversion
fn rgb_to_hsv(rgb: vec3<f32>) -> vec3<f32> {
    let max_val = max(max(rgb.r, rgb.g), rgb.b);
    let min_val = min(min(rgb.r, rgb.g), rgb.b);
    let delta = max_val - min_val;

    var hsv: vec3<f32>;
    hsv.z = max_val;  // Value

    if (delta == 0.0) {
        hsv.x = 0.0;  // Hue undefined
        hsv.y = 0.0;  // Saturation
    } else {
        hsv.y = delta / max_val;  // Saturation

        if (rgb.r == max_val) {
            hsv.x = (rgb.g - rgb.b) / delta;
        } else if (rgb.g == max_val) {
            hsv.x = 2.0 + (rgb.b - rgb.r) / delta;
        } else {
            hsv.x = 4.0 + (rgb.r - rgb.g) / delta;
        }

        hsv.x = hsv.x / 6.0;
        if (hsv.x < 0.0) {
            hsv.x += 1.0;
        }
    }

    return hsv;
}

// HSV to RGB conversion
fn hsv_to_rgb(hsv: vec3<f32>) -> vec3<f32> {
    let h = hsv.x * 6.0;
    let s = hsv.y;
    let v = hsv.z;

    let i = floor(h);
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));

    let mod_i = i32(i) % 6;

    if (mod_i == 0) {
        return vec3<f32>(v, t, p);
    } else if (mod_i == 1) {
        return vec3<f32>(q, v, p);
    } else if (mod_i == 2) {
        return vec3<f32>(p, v, t);
    } else if (mod_i == 3) {
        return vec3<f32>(p, q, v);
    } else if (mod_i == 4) {
        return vec3<f32>(t, p, v);
    } else {
        return vec3<f32>(v, p, q);
    }
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    let color = textureLoad(input_texture, coord, 0);

    // Convert to HSV
    var hsv = rgb_to_hsv(color.rgb);

    // Shift hue
    hsv.x = fract(hsv.x + HUE_SHIFT);

    // Convert back to RGB
    let rgb = hsv_to_rgb(hsv);

    textureStore(output_texture, coord, vec4<f32>(rgb, color.a));
}
"""