"""Windows audio capture using WASAPI (PLACEHOLDER - Implementation only, not tested)."""

class WASAPICapture:
    """
    Windows audio capture using WASAPI.

    PLACEHOLDER: Implementation only, user will test later.
    Same API as CoreAudioCapture.
    """

    def __init__(self, gpu_context, sample_rate, chunk_size, device_name=None, process_callback=None):
        raise NotImplementedError("WASAPI audio capture not yet implemented")

    def start(self):
        raise NotImplementedError("WASAPI audio capture not yet implemented")

    def stop(self):
        pass

    @property
    def chunks_captured(self):
        return 0

    @property
    def chunks_dropped(self):
        return 0
