"""Linux display output using Vulkan/X11 (PLACEHOLDER - Not yet implemented)."""

class DisplayWindow:
    """
    Linux display window using Vulkan/X11.

    PLACEHOLDER: Not yet implemented.
    Will provide same API as macOS DisplayWindow.
    """

    def __init__(self, gpu_context, width=1920, height=1080, title="streamlib"):
        raise NotImplementedError("Linux display not yet implemented")

    def get_render_texture(self):
        """Get wgpu texture to render into."""
        raise NotImplementedError("Linux display not yet implemented")

    def present(self):
        """Present the rendered frame to the display."""
        raise NotImplementedError("Linux display not yet implemented")

    def close(self):
        """Close the display window."""
        pass
