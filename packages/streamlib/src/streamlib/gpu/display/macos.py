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
