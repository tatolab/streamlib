"""Linux camera capture using V4L2 (PLACEHOLDER - Implementation only, not tested)."""

class V4L2Capture:
    """
    Linux camera capture using V4L2.

    PLACEHOLDER: Implementation only, user will test later.
    Same API as AVFoundationCapture.
    """

    def __init__(self, gpu_context, runtime_width, runtime_height, device_id=None):
        raise NotImplementedError("V4L2 camera capture not yet implemented")

    def get_texture(self):
        raise NotImplementedError("V4L2 camera capture not yet implemented")

    def stop(self):
        pass
