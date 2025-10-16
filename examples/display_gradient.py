"""Display a blue-green gradient in a window using WebGPU."""
import asyncio
import wgpu
from streamlib.gpu import GPUContext
from streamlib.gpu.display import DisplayWindow
from Cocoa import NSApplication, NSDate, NSDefaultRunLoopMode, NSEventMaskAny
from Foundation import NSAutoreleasePool


GRADIENT_SHADER = """
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    // Fullscreen triangle
    let x = f32((vertex_index << 1u) & 2u);
    let y = f32(vertex_index & 2u);
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Blue-green gradient: blue at top (y=0), green at bottom (y=1)
    let blue = vec3<f32>(0.0, 0.5, 1.0);   // Bright blue
    let green = vec3<f32>(0.0, 1.0, 0.5);  // Bright green

    // Interpolate between blue and green based on vertical position
    let color = mix(blue, green, in.uv.y);

    return vec4<f32>(color, 1.0);
}
"""


async def main():
    print("Creating GPU context...")
    ctx = await GPUContext.create()
    print(f"✅ Using {ctx.backend_name} on {ctx.device_name}")

    print("\nCreating display window...")
    window = DisplayWindow(ctx, width=1280, height=720, title="Blue-Green Gradient")
    print(f"✅ Window created: {window.width}x{window.height}")

    print("\nCreating render pipeline...")
    # Create shader module
    shader_module = ctx.device.create_shader_module(code=GRADIENT_SHADER)

    # Create render pipeline
    pipeline = ctx.device.create_render_pipeline(
        layout=ctx.device.create_pipeline_layout(bind_group_layouts=[]),
        vertex={
            "module": shader_module,
            "entry_point": "vs_main"
        },
        fragment={
            "module": shader_module,
            "entry_point": "fs_main",
            "targets": [{"format": wgpu.TextureFormat.bgra8unorm}]
        },
        primitive={"topology": "triangle-list"}
    )
    print("✅ Render pipeline created")

    print("\n🎨 Rendering gradient...")
    print("Press Ctrl+C to close window\n")

    # Activate the application
    app = NSApplication.sharedApplication()
    app.activateIgnoringOtherApps_(True)

    frame_count = 0

    try:
        while window.is_open():
            # Process macOS events (needed for window to appear and respond)
            pool = NSAutoreleasePool.alloc().init()

            event = app.nextEventMatchingMask_untilDate_inMode_dequeue_(
                NSEventMaskAny,
                NSDate.dateWithTimeIntervalSinceNow_(0),
                NSDefaultRunLoopMode,
                True
            )
            if event:
                app.sendEvent_(event)
                app.updateWindows()

            del pool

            # Get current swapchain texture
            texture = window.get_current_texture()

            # Create command encoder
            encoder = ctx.device.create_command_encoder()

            # Begin render pass
            render_pass = encoder.begin_render_pass(
                color_attachments=[{
                    "view": texture.create_view(),
                    "clear_value": (0, 0, 0, 1),
                    "load_op": "clear",
                    "store_op": "store",
                }]
            )

            # Draw fullscreen triangle (gradient)
            render_pass.set_pipeline(pipeline)
            render_pass.draw(3, 1, 0, 0)  # 3 vertices for fullscreen triangle
            render_pass.end()

            # Submit commands
            ctx.device.queue.submit([encoder.finish()])

            # Present frame
            window.present()

            frame_count += 1
            if frame_count % 60 == 0:
                print(f"Frame {frame_count}")

            # Small delay to avoid busy loop
            await asyncio.sleep(1/60)  # ~60 FPS

    except KeyboardInterrupt:
        print("\n\n⏹️  Interrupted by user")

    print(f"✅ Rendered {frame_count} frames")
    window.close()


if __name__ == "__main__":
    asyncio.run(main())
