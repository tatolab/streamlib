"""
Camera with animated bouncing ball overlay using @processor decorator.

This demonstrates:
- Camera → Python Processor → Display pipeline
- WebGPU shader for drawing animated graphics
- Bouncing ball physics simulation
- GPU-based compositing

Zero-copy GPU pipeline:
    Camera → WebGPU texture → Bouncing ball shader → Display swapchain
"""

import time
import struct
import random
from streamlib import camera_processor, processor, display_processor, StreamRuntime, StreamInput, StreamOutput, VideoFrame


# WGSL shader - reads ball parameters from uniform buffer
BOUNCING_BALL_SHADER = """
struct BallParams {
    x: f32,
    y: f32,
    radius: f32,
    _padding: f32,
}

@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> ball: BallParams;

const BALL_COLOR: vec4<f32> = vec4<f32>(1.0, 0.3, 0.2, 1.0);  // Orange-red

// Draw a smooth anti-aliased circle using signed distance field
fn circle_sdf(pos: vec2<f32>, center: vec2<f32>, radius: f32) -> f32 {
    return length(pos - center) - radius;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_texture);
    let coord = vec2<i32>(gid.xy);

    if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
        return;
    }

    // Load camera frame color
    let camera_color = textureLoad(input_texture, coord, 0);

    // Read ball parameters from uniform buffer
    let ball_x = ball.x;
    let ball_y = ball.y;
    let ball_radius = ball.radius;

    // Normalize pixel coordinates (0.0 to 1.0)
    let uv = vec2<f32>(f32(coord.x) / f32(dims.x), f32(coord.y) / f32(dims.y));

    // Calculate aspect ratio correction
    let aspect = f32(dims.x) / f32(dims.y);
    let uv_corrected = vec2<f32>(uv.x * aspect, uv.y);
    let ball_pos = vec2<f32>(ball_x * aspect, ball_y);

    // Calculate distance to ball center using SDF
    let dist = circle_sdf(uv_corrected, ball_pos, ball_radius);

    // Anti-aliased edge (smooth transition over 2 pixels)
    let edge_width = 0.002;
    let alpha = 1.0 - smoothstep(-edge_width, edge_width, dist);

    // Composite ball on top of camera feed (alpha blending)
    let ball_with_alpha = vec4<f32>(BALL_COLOR.rgb, BALL_COLOR.a * alpha);
    let result = mix(camera_color, ball_with_alpha, ball_with_alpha.a);

    textureStore(output_texture, coord, result);
}
"""


# Ball physics state
class BouncingBall:
    """Simple bouncing ball physics simulation."""

    def __init__(self, width: float = 1.0, height: float = 1.0):
        # Normalized coordinates (0.0 to 1.0)
        self.x = 0.5
        self.y = 0.5
        self.vx = 0.3  # Velocity in normalized coords per second
        self.vy = 0.4
        self.radius = 0.05  # Normalized radius
        self.width = width
        self.height = height
        self.gravity = 0.8  # Gravity acceleration
        self.bounce_damping = 0.85  # Energy loss on bounce

    def update(self, dt: float):
        """Update ball position and handle bouncing."""
        # Apply gravity
        self.vy += self.gravity * dt

        # Update position
        self.x += self.vx * dt
        self.y += self.vy * dt

        # Bounce off walls (left/right)
        if self.x - self.radius < 0:
            self.x = self.radius
            self.vx = abs(self.vx) * self.bounce_damping
        elif self.x + self.radius > self.width:
            self.x = self.width - self.radius
            self.vx = -abs(self.vx) * self.bounce_damping

        # Bounce off floor/ceiling
        if self.y - self.radius < 0:
            self.y = self.radius
            self.vy = abs(self.vy) * self.bounce_damping
        elif self.y + self.radius > self.height:
            self.y = self.height - self.radius
            self.vy = -abs(self.vy) * self.bounce_damping

        # Add small random perturbation to keep it interesting
        if random.random() < 0.01:  # 1% chance per frame
            self.vx += random.uniform(-0.1, 0.1)
            self.vy += random.uniform(-0.1, 0.1)


@camera_processor(device_id="0x1424001bcf2284")  # None = first available camera
def camera():
    """Zero-copy camera source - no code needed!"""
    pass


@processor(
    description="GPU-accelerated bouncing ball overlay effect",
    usage_context="Draws an animated bouncing ball on video frames using WebGPU compute shaders",
    tags=["gpu", "effect", "animation", "demo"]
)
class BouncingBallOverlay:
    """
    GPU-accelerated bouncing ball overlay processor.

    Draws an animated bouncing ball on top of the camera feed using WebGPU.
    Ball physics are calculated on CPU, rendering done on GPU.
    """

    class InputPorts:
        video = StreamInput(VideoFrame)

    class OutputPorts:
        video = StreamOutput(VideoFrame)

    def __init__(self):
        print("BouncingBallOverlay.__init__() called")
        self.ball = BouncingBall()
        self.last_time = time.perf_counter()
        self.pipeline = None
        self.uniform_buffer = None
        self.initialized = False

    def _initialize_gpu(self, gpu, width, height):
        """Initialize GPU resources (called on first frame)."""
        try:
            import wgpu

            print(f"Initializing GPU resources for {width}x{height}")

            # Create shader module
            print("Creating shader module...")
            shader_module = gpu.device.create_shader_module(code=BOUNCING_BALL_SHADER)
            print("Shader module created")

            # Create bind group layout with 3 bindings (input, output, uniform buffer)
            print("Creating bind group layout...")
            bind_group_layout = gpu.device.create_bind_group_layout(
                entries=[
                    {
                        "binding": 0,
                        "visibility": wgpu.ShaderStage.COMPUTE,
                        "texture": {
                            "sample_type": wgpu.TextureSampleType.float,
                            "view_dimension": wgpu.TextureViewDimension.d2,
                        }
                    },
                    {
                        "binding": 1,
                        "visibility": wgpu.ShaderStage.COMPUTE,
                        "storage_texture": {
                            "access": wgpu.StorageTextureAccess.write_only,
                            "format": wgpu.TextureFormat.rgba8unorm,
                            "view_dimension": wgpu.TextureViewDimension.d2,
                        }
                    },
                    {
                        "binding": 2,
                        "visibility": wgpu.ShaderStage.COMPUTE,
                        "buffer": {
                            "type": wgpu.BufferBindingType.uniform,
                        }
                    }
                ]
            )
            print("Bind group layout created")

            # Create pipeline layout
            print("Creating pipeline layout...")
            pipeline_layout = gpu.device.create_pipeline_layout(
                bind_group_layouts=[bind_group_layout]
            )
            print("Pipeline layout created")

            # Create compute pipeline
            print("Creating compute pipeline...")
            self.pipeline = gpu.device.create_compute_pipeline(
                layout=pipeline_layout,
                compute={
                    "module": shader_module,
                    "entry_point": "main",
                }
            )
            print("Compute pipeline created")

            # Store bind group layout for later use
            self.bind_group_layout = bind_group_layout

            # Create uniform buffer for ball parameters (16 bytes: 4 floats)
            print("Creating uniform buffer...")
            self.uniform_buffer = gpu.device.create_buffer(
                size=16,  # 4 floats * 4 bytes = 16 bytes
                usage=wgpu.BufferUsage.UNIFORM | wgpu.BufferUsage.COPY_DST
            )
            print("Uniform buffer created")

            self.initialized = True
            print("GPU resources initialized successfully")

        except Exception as e:
            print(f"❌ ERROR in _initialize_gpu: {type(e).__name__}: {e}")
            import traceback
            traceback.print_exc()
            raise

    def process(self, tick):
        """Process each frame: update physics and render ball."""
        # Read input frame
        frame = self.input_ports().video.read_latest()
        if not frame:
            return

        # Get GPU context
        gpu = self.gpu_context()

        # Initialize GPU resources on first frame
        if not self.initialized:
            self._initialize_gpu(gpu, frame.width, frame.height)

        # Update ball physics
        current_time = time.perf_counter()
        dt = current_time - self.last_time
        self.last_time = current_time
        self.ball.update(dt)

        # Update uniform buffer with ball position (fast GPU upload)
        params_data = struct.pack('ffff', self.ball.x, self.ball.y, self.ball.radius, 0.0)
        gpu.queue.write_buffer(self.uniform_buffer, 0, params_data)

        # Create output texture
        output = gpu.create_texture(frame.width, frame.height)

        # Create bind group
        bind_group = gpu.device.create_bind_group(
            layout=self.bind_group_layout,
            entries=[
                {"binding": 0, "resource": frame.data.create_view()},
                {"binding": 1, "resource": output.create_view()},
                {"binding": 2, "resource": {"buffer": self.uniform_buffer}},
            ]
        )

        # Calculate workgroup count (8x8 workgroup size)
        workgroup_count_x = (frame.width + 7) // 8
        workgroup_count_y = (frame.height + 7) // 8

        # Create command encoder and run compute pass
        encoder = gpu.device.create_command_encoder()
        compute_pass = encoder.begin_compute_pass()
        compute_pass.set_pipeline(self.pipeline)
        compute_pass.set_bind_group(0, bind_group)
        compute_pass.dispatch_workgroups(workgroup_count_x, workgroup_count_y, 1)
        compute_pass.end()

        # Submit
        gpu.queue.submit([encoder.finish()])

        # Write output frame
        output_frame = frame.clone_with_texture(output)
        self.output_ports().video.write(output_frame)


@display_processor(title="Camera with Bouncing Ball - streamlib")
def display():
    """Zero-copy display sink - no code needed!"""
    pass


def main():
    print("🎥 Starting camera-to-display pipeline with bouncing ball overlay...")
    print("Press Ctrl+C to stop\n")

    # Create runtime (60 FPS for smooth animation, 1920x1080)
    runtime = StreamRuntime(fps=60, width=1920, height=1080, enable_gpu=True)

    # Add processors to runtime
    runtime.add_stream(camera)
    runtime.add_stream(BouncingBallOverlay)
    runtime.add_stream(display)

    # Connect pipeline: camera → bouncing_ball_overlay → display
    runtime.connect(camera.output_ports().video, BouncingBallOverlay.input_ports().video)
    runtime.connect(BouncingBallOverlay.output_ports().video, display.input_ports().video)

    # Start the pipeline and run until interrupted
    print("✅ Pipeline configured: Camera → Bouncing Ball (GPU) → Display")
    print("✅ Starting runtime...\n")

    runtime.run()


if __name__ == "__main__":
    main()
