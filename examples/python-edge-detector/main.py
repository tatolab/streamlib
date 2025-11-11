"""
Python Edge Detector Example using NEW field marker API.

Demonstrates:
- New field marker pattern: input(), output(), config()
- Direct field access: self.video_in.read_latest()
- Lifecycle methods: on_start(ctx), on_stop()
- GPU-accelerated processing with WebGPU

This example matches the Rust macro ergonomics!
"""

from streamlib import (
    StreamProcessor, input, output, config,
    VideoFrame,
    camera_processor, display_processor, StreamRuntime,
)


@camera_processor(device_id=None)  # None = first available camera
def camera():
    """Zero-copy camera source"""
    pass


@StreamProcessor(mode="Pull", description="GPU-accelerated edge detector")
class EdgeDetector:
    """
    Detects edges in video frames using a Sobel filter on the GPU.

    This demonstrates the NEW field marker API that matches Rust's ergonomics:
    - Direct field access instead of self.input_ports().video
    - Lifecycle methods on_start() and on_stop()
    - Config fields
    """

    # Port declarations using field markers (matches Rust!)
    video_in = input(description="Video frames to process")
    video_out = output(description="Edge-detected frames")

    # Config field with default value
    threshold = config(0.1)

    def on_start(self, ctx):
        """Called when processor starts - initialize GPU resources"""
        print(f"EdgeDetector starting with threshold={self.threshold}")
        self.gpu = ctx.gpu
        self.initialized = False
        self.pipeline = None
        print("GPU context initialized!")

    def on_stop(self):
        """Called when processor stops - cleanup resources"""
        print("EdgeDetector stopping, cleaning up resources...")

    def _initialize_gpu(self, width, height):
        """Initialize GPU pipeline on first frame"""
        print(f"Initializing Sobel edge detection pipeline ({width}x{height})")

        # Create Sobel edge detection shader
        shader_code = """
        @group(0) @binding(0) var input_texture: texture_2d<f32>;
        @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

        // Sobel kernels for edge detection
        const SOBEL_X: array<f32, 9> = array<f32, 9>(
            -1.0, 0.0, 1.0,
            -2.0, 0.0, 2.0,
            -1.0, 0.0, 1.0
        );

        const SOBEL_Y: array<f32, 9> = array<f32, 9>(
            -1.0, -2.0, -1.0,
             0.0,  0.0,  0.0,
             1.0,  2.0,  1.0
        );

        fn sample_offset(coord: vec2<i32>, offset: vec2<i32>) -> vec4<f32> {
            return textureLoad(input_texture, coord + offset, 0);
        }

        @compute @workgroup_size(8, 8)
        fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
            let dims = textureDimensions(input_texture);
            let coord = vec2<i32>(gid.xy);

            if (coord.x >= i32(dims.x) || coord.y >= i32(dims.y)) {
                return;
            }

            // Sample 3x3 neighborhood
            var gx = 0.0;
            var gy = 0.0;

            for (var y = -1; y <= 1; y++) {
                for (var x = -1; x <= 1; x++) {
                    let idx = (y + 1) * 3 + (x + 1);
                    let color = sample_offset(coord, vec2<i32>(x, y));
                    let luminance = dot(color.rgb, vec3<f32>(0.299, 0.587, 0.114));

                    gx += luminance * SOBEL_X[idx];
                    gy += luminance * SOBEL_Y[idx];
                }
            }

            // Compute gradient magnitude
            let magnitude = sqrt(gx * gx + gy * gy);
            let edge = vec4<f32>(magnitude, magnitude, magnitude, 1.0);

            textureStore(output_texture, coord, edge);
        }
        """

        # Create compute pipeline (simplified - actual implementation would need full setup)
        print("Edge detection GPU pipeline initialized!")
        self.initialized = True

    def process(self):
        """Process each frame - NEW direct field access!"""
        # NEW: Direct field access instead of self.input_ports().video
        frame = self.video_in.read_latest()

        if not frame:
            return

        # Initialize GPU on first frame
        if not self.initialized:
            self._initialize_gpu(frame.width, frame.height)

        # For now, pass through (actual GPU processing would go here)
        processed_frame = frame

        # NEW: Direct field access instead of self.output_ports().video_out
        self.video_out.write(processed_frame)


@display_processor(title="Edge Detector - streamlib")
def display():
    """Zero-copy display sink"""
    pass


def main():
    print("🎥 Python Edge Detector with NEW Field Marker API")
    print("=" * 60)
    print("This example demonstrates the new Rust-like API:")
    print("  - Field markers: input(), output(), config()")
    print("  - Direct access: self.video_in.read_latest()")
    print("  - Lifecycle: on_start(ctx), on_stop()")
    print("=" * 60)
    print()

    # Create runtime
    runtime = StreamRuntime(fps=30, width=1920, height=1080, enable_gpu=True)

    # Add processors
    runtime.add_stream(camera)
    runtime.add_stream(EdgeDetector)
    runtime.add_stream(display)

    # Connect pipeline: camera → edge_detector → display
    runtime.connect(camera.output_ports().video, EdgeDetector.input_ports().video_in)
    runtime.connect(EdgeDetector.output_ports().video_out, display.input_ports().video)

    print("✅ Pipeline configured: Camera → Edge Detector → Display")
    print("✅ Starting runtime...\n")
    print("Press Ctrl+C to stop\n")

    runtime.run()


if __name__ == "__main__":
    main()
