"""Windows display output using DirectX/Win32 (PLACEHOLDER - Not yet implemented)."""

class DisplayWindow:
    """
    Windows display window using DirectX/Win32.

    PLACEHOLDER: Not yet implemented.
    Will provide same API as macOS DisplayWindow.
    """

    def __init__(self, gpu_context, width=1920, height=1080, title="streamlib"):
        raise NotImplementedError("Windows display not yet implemented")

    def get_render_texture(self):
        """Get wgpu texture to render into."""
        raise NotImplementedError("Windows display not yet implemented")

    def present(self):
        """Present the rendered frame to the display."""
        raise NotImplementedError("Windows display not yet implemented")

    def close(self):
        """Close the display window."""
        pass
