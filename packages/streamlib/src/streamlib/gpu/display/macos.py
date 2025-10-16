"""macOS display output using zero-copy wgpu presentation context pipeline.

This module provides DisplayWindow which uses wgpu's built-in presentation context:
- Creates NSWindow and CAMetalLayer via PyObjC
- Uses WgpuCanvas to get presentation context
- Context manages swapchain textures (backed by CAMetalLayer drawables - zero-copy!)

Pipeline:
1. User gets current swapchain texture via get_current_texture()
2. User renders to the swapchain texture
3. User calls present() to display the frame
4. wgpu internally presents to CAMetalLayer (zero-copy!)

Note: True zero-copy - wgpu presentation context textures are directly backed by CAMetalLayer drawables.
"""

import wgpu
from wgpu.gui import WgpuCanvasBase
from Cocoa import (
    NSWindow, NSView, NSBackingStoreBuffered, NSApplication,
    NSApplicationActivationPolicyRegular
)
from Quartz import CAMetalLayer
import objc


class DisplayWindow:
    """
    macOS display window using wgpu Surface/SwapChain (zero-copy).

    Architecture:
    1. Creates NSWindow and CAMetalLayer via PyObjC
    2. Creates wgpu Surface from the CAMetalLayer
    3. Creates wgpu SwapChain from the Surface
    4. User renders to swapchain textures
    5. User calls present() - wgpu handles zero-copy presentation internally

    Usage:
        window = DisplayWindow(gpu_context, width=1920, height=1080)

        # Get the current swapchain texture
        texture = window.get_current_texture()

        # Render to texture...
        render_to_texture(gpu_context, texture)

        # Present to display (zero-copy!)
        window.present()
    """

    def __init__(self, gpu_context, width=1920, height=1080, title="streamlib"):
        """
        Create display window with wgpu Surface/SwapChain.

        Args:
            gpu_context: GPUContext instance with device and adapter
            width: Window width in pixels
            height: Window height in pixels
            title: Window title
        """
        self.gpu_context = gpu_context
        self.width = width
        self.height = height
        self.title = title

        # Initialize NSApplication if not already initialized
        app = NSApplication.sharedApplication()
        app.setActivationPolicy_(NSApplicationActivationPolicyRegular)

        # Create NSWindow
        self.ns_window = NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(
            ((100, 100), (width, height)),
            15,  # NSTitledWindowMask | NSClosableWindowMask | NSMiniaturizableWindowMask | NSResizableWindowMask
            NSBackingStoreBuffered,
            False
        )
        self.ns_window.setTitle_(title)

        # Create CAMetalLayer
        self.metal_layer = CAMetalLayer.layer()
        self.metal_layer.setPixelFormat_(80)  # MTLPixelFormatBGRA8Unorm
        self.metal_layer.setDrawableSize_((width, height))

        # Create NSView and attach layer
        self.content_view = NSView.alloc().initWithFrame_(((0, 0), (width, height)))
        self.content_view.setWantsLayer_(True)
        self.content_view.setLayer_(self.metal_layer)

        # Set window content and show
        self.ns_window.setContentView_(self.content_view)
        self.ns_window.makeKeyAndOrderFront_(None)

        # Create wgpu canvas wrapper
        self._canvas = _MetalLayerCanvas(self.content_view, width, height)

        # Get presentation context from canvas (wgpu context)
        self.context = self._canvas.get_context("wgpu")

        # Configure the presentation context
        self.context.configure(
            device=gpu_context.device,
            format=wgpu.TextureFormat.bgra8unorm,
        )

        # Track current texture
        self._current_texture = None

        # Store NSApplication for event processing
        self._app = app
        self._NSDate = None
        self._NSDefaultRunLoopMode = None
        self._NSEventMaskAny = None

    def process_events(self):
        """
        Process macOS window events (NSApplication event loop).

        This must be called regularly (e.g., every frame) to keep the window
        responsive and handle user input.

        Example:
            window.process_events()
        """
        # Lazy import to avoid issues if called from wrong thread
        if self._NSDate is None:
            from Cocoa import NSDate, NSDefaultRunLoopMode, NSEventMaskAny
            self._NSDate = NSDate
            self._NSDefaultRunLoopMode = NSDefaultRunLoopMode
            self._NSEventMaskAny = NSEventMaskAny

        # Process all pending events (non-blocking)
        event = self._app.nextEventMatchingMask_untilDate_inMode_dequeue_(
            self._NSEventMaskAny,
            self._NSDate.dateWithTimeIntervalSinceNow_(0),  # Non-blocking (0 timeout)
            self._NSDefaultRunLoopMode,
            True
        )

        if event:
            self._app.sendEvent_(event)
            self._app.updateWindows()

    def get_current_texture(self):
        """
        Get the current swapchain texture to render into.

        Returns:
            wgpu.GPUTexture: The current swapchain texture (backed by CAMetalLayer drawable)

        Note:
            This texture is directly backed by a CAMetalLayer drawable.
            Rendering to it is zero-copy!
        """
        self._current_texture = self.context.get_current_texture()
        return self._current_texture

    def present(self):
        """
        Present the current frame to the display (zero-copy).

        This calls wgpu's present() which internally:
        1. Completes any pending GPU work
        2. Presents the CAMetalLayer drawable
        3. All on GPU, no CPU copies!

        Note:
            Must be called after rendering to the texture from get_current_texture().
        """
        if self._current_texture is not None:
            self.context.present()
            self._current_texture = None

    def render(self, texture):
        """
        Render a texture to the display (convenience method).

        This is a simplified API that renders the texture to the swapchain,
        presents it, and processes window events. All the platform-specific
        boilerplate (NSApplication events on macOS, etc.) is handled automatically.

        Args:
            texture: WebGPU texture to display

        Example:
            # Simple rendering - events processed automatically!
            window.render(frame.data)
        """
        # Process window events (NSApplication on macOS)
        # This keeps the window responsive and handles user input
        self.process_events()

        # Get swapchain texture
        dst_texture = self.get_current_texture()

        # Create blit pipeline if not already created
        if not hasattr(self, '_blit_pipeline'):
            self._create_blit_pipeline()

        # Create bind group for this texture
        bind_group = self.gpu_context.device.create_bind_group(
            layout=self._blit_pipeline.get_bind_group_layout(0),
            entries=[
                {
                    "binding": 0,
                    "resource": texture.create_view()
                },
                {
                    "binding": 1,
                    "resource": self._sampler
                }
            ]
        )

        # Render texture to swapchain using blit pipeline
        encoder = self.gpu_context.device.create_command_encoder()
        render_pass = encoder.begin_render_pass(
            color_attachments=[{
                "view": dst_texture.create_view(),
                "load_op": "clear",
                "store_op": "store",
                "clear_value": (0, 0, 0, 1)
            }]
        )
        render_pass.set_pipeline(self._blit_pipeline)
        render_pass.set_bind_group(0, bind_group)
        render_pass.draw(3)  # Fullscreen triangle
        render_pass.end()
        self.gpu_context.queue.submit([encoder.finish()])

        # Present
        self.present()

    def _create_blit_pipeline(self):
        """Create a simple blit pipeline for rendering textures to the screen."""
        # Vertex shader - fullscreen triangle
        vertex_shader = """
        @vertex
        fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
            // Fullscreen triangle
            var pos = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0)
            );
            return vec4<f32>(pos[vertex_index], 0.0, 1.0);
        }
        """

        # Fragment shader - sample texture
        fragment_shader = """
        @group(0) @binding(0) var input_texture: texture_2d<f32>;
        @group(0) @binding(1) var input_sampler: sampler;

        @fragment
        fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
            let tex_size = textureDimensions(input_texture);
            let uv = pos.xy / vec2<f32>(f32(tex_size.x), f32(tex_size.y));
            return textureSample(input_texture, input_sampler, uv);
        }
        """

        # Create shader module
        shader_module = self.gpu_context.device.create_shader_module(
            code=vertex_shader + "\n" + fragment_shader
        )

        # Create sampler
        self._sampler = self.gpu_context.device.create_sampler(
            mag_filter="linear",
            min_filter="linear"
        )

        # Create pipeline
        self._blit_pipeline = self.gpu_context.device.create_render_pipeline(
            layout="auto",
            vertex={
                "module": shader_module,
                "entry_point": "vs_main"
            },
            fragment={
                "module": shader_module,
                "entry_point": "fs_main",
                "targets": [{
                    "format": wgpu.TextureFormat.bgra8unorm
                }]
            },
            primitive={
                "topology": "triangle-list"
            }
        )

    def close(self):
        """Close the display window and clean up resources."""
        self.ns_window.close()

    def is_open(self):
        """Check if window is still open."""
        return self.ns_window.isVisible()

    def set_title(self, title):
        """Update window title."""
        self.ns_window.setTitle_(title)


class _MetalLayerCanvas(WgpuCanvasBase):
    """Internal helper to wrap NSView as a wgpu canvas."""

    def __init__(self, ns_view, width, height):
        super().__init__()
        self.ns_view = ns_view
        self._width = width
        self._height = height

    def get_physical_size(self):
        """Return the physical size of the canvas."""
        return self._width, self._height

    def get_pixel_ratio(self):
        """Return the pixel ratio (1.0 for now, could use retina scaling)."""
        return 1.0

    def get_logical_size(self):
        """Return the logical size of the canvas."""
        return self._width, self._height

    def _get_window_id(self):
        """Return the window ID (NSView pointer) for wgpu-native."""
        # For Metal on macOS, this is the NSView pointer
        # Use objc.pyobjc_id to get the pointer as an integer
        return int(objc.pyobjc_id(self.ns_view))

    def _rc_get_present_methods(self):
        """Return the present methods supported by this canvas."""
        # For macOS with Metal, we support the standard present method
        return {
            "get_surface_info": self._get_surface_info,
            "present_image": self._present_image,
            "bitmap": {
                "formats": ["rgba8unorm", "bgra8unorm"],
            },
            "screen": {
                "formats": ["rgba8unorm", "bgra8unorm"],
                "window": self._get_window_id(),
                "platform": "macos",
                "display": None,
            },
        }

    def _get_surface_info(self):
        """Return surface info for wgpu-native."""
        return {
            "window": self._get_window_id(),
            "platform": "macos",
            "display": None,
        }

    def _present_image(self, **kwargs):
        """Present an image (no-op for wgpu since it handles presentation)."""
        # wgpu handles presentation internally via the swapchain
        pass

    def set_logical_size(self, width, height):
        """Set the logical size of the canvas."""
        self._width = width
        self._height = height

    def close(self):
        """Close the canvas."""
        pass

    def is_closed(self):
        """Check if the canvas is closed."""
        return False
