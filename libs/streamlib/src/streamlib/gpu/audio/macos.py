"""macOS audio capture using Core Audio (via sounddevice/PortAudio).

This module provides CoreAudioCapture which:
- Captures audio from macOS devices using Core Audio framework
- Processes audio in real-time chunks (default 512 samples @ 48kHz = ~10.7ms)
- Calls process_callback for each chunk (user provides GPU processing)
- Integrates with streamlib's GPU-first architecture
- Thread-safe operation with minimal overhead

Real-time streaming pipeline:
1. Audio device → sounddevice callback (background thread)
2. Convert to Float32 mono
3. Call user's process_callback (GPU effect processing happens here)
4. Track stats (chunks captured, dropped)

This matches the proven architecture from examples/audio_streaming.py.
"""

import threading
import numpy as np
import sounddevice as sd
from typing import Optional, Callable


class CoreAudioCapture:
    """
    macOS audio capture using Core Audio (via sounddevice).

    Captures audio in real-time chunks and calls process_callback for each chunk.
    The callback receives a numpy array and should return a numpy array.

    Uses PortAudio/Core Audio underneath for low-latency audio I/O.
    """

    def __init__(
        self,
        gpu_context,
        sample_rate: int = 48000,
        chunk_size: int = 512,
        device_name: Optional[str] = None,
        process_callback: Optional[Callable[[np.ndarray], np.ndarray]] = None
    ):
        """
        Args:
            gpu_context: GPUContext instance
            sample_rate: Sample rate in Hz (default 48000)
            chunk_size: Samples per chunk (default 512 = ~10.7ms @ 48kHz)
            device_name: Device name substring to match (None = default device)
            process_callback: Function(chunk: np.ndarray) -> np.ndarray
                             Called for each audio chunk on background thread
        """
        self.gpu_context = gpu_context
        self.sample_rate = sample_rate
        self.chunk_size = chunk_size
        self.process_callback = process_callback

        # Find device
        self.device_id, self.device_info = self._find_device(device_name)

        # Stats
        self._chunks_captured = 0
        self._chunks_dropped = 0
        self._lock = threading.Lock()

        # Stream (created on start)
        self._stream = None
        self._running = False

    def _find_device(self, device_name: Optional[str]) -> tuple[Optional[int], Optional[dict]]:
        """Find audio input device by name substring.

        Returns:
            Tuple of (device_id, device_info) where device_info contains
            channel count, sample rate, etc.
        """
        if device_name is None:
            # Use default input device
            default_device = sd.default.device[0]
            devices = sd.query_devices()
            if default_device is not None and devices[default_device]['max_input_channels'] > 0:
                device_info = devices[default_device]
                print(f"📱 Using default audio device: {device_info['name']} (id={default_device})")
                print(f"   Channels: {device_info['max_input_channels']}, Sample Rate: {device_info['default_samplerate']:.0f}Hz")
                return default_device, device_info
            return None, None

        # Query all devices
        devices = sd.query_devices()

        # Find all matching input devices
        matches = []
        for idx, device in enumerate(devices):
            if (device['max_input_channels'] > 0 and
                device_name.lower() in device['name'].lower()):
                matches.append((idx, device['name']))

        if not matches:
            # Not found - show available devices
            print(f"❌ Audio device '{device_name}' not found.")
            print("\n📱 Available Audio Input Devices:")
            for idx, device in enumerate(devices):
                if device["max_input_channels"] > 0:
                    default = " (default)" if idx == sd.default.device[0] else ""
                    print(
                        f"  [{idx}] {device['name']}{default} "
                        f"({device['max_input_channels']} channels, "
                        f"{device['default_samplerate']:.0f}Hz)"
                    )
            raise RuntimeError(
                f"Audio device '{device_name}' not found. "
                f"See available devices above."
            )

        # If multiple matches, show warning
        if len(matches) > 1:
            print(f"⚠️  Multiple devices match '{device_name}':")
            for idx, name in matches:
                print(f"     [{idx}] {name}")
            print(f"📱 Using first match: {matches[0][1]} (id={matches[0][0]})")
        else:
            print(f"📱 Found audio device: {matches[0][1]} (id={matches[0][0]})")

        # Get device info for selected device
        device_id = matches[0][0]
        device_info = devices[device_id]
        print(f"   Channels: {device_info['max_input_channels']}, Sample Rate: {device_info['default_samplerate']:.0f}Hz")

        return device_id, device_info

    def _capture_callback(self, indata, frames, time_info, status):
        """
        Called by sounddevice on background thread for each audio chunk.

        Args:
            indata: numpy array (frames, channels) - Float32 samples
            frames: Number of frames (should equal chunk_size)
            time_info: Timing info
            status: Status flags
        """
        if status:
            print(f"⚠️  Audio status: {status}")

        try:
            # Convert to mono if stereo (average channels)
            if indata.shape[1] > 1:
                audio_chunk = indata.mean(axis=1).astype(np.float32)
            else:
                audio_chunk = indata[:, 0].astype(np.float32)

            # Ensure correct size
            if len(audio_chunk) != self.chunk_size:
                print(f"⚠️  Chunk size mismatch: got {len(audio_chunk)}, expected {self.chunk_size}")
                with self._lock:
                    self._chunks_dropped += 1
                return

            # Call user's processing callback
            if self.process_callback:
                try:
                    processed_chunk = self.process_callback(audio_chunk)

                    # Validate output
                    if not isinstance(processed_chunk, np.ndarray):
                        raise TypeError(f"Callback must return np.ndarray, got {type(processed_chunk)}")
                    if len(processed_chunk) != self.chunk_size:
                        raise ValueError(
                            f"Callback returned wrong size: {len(processed_chunk)} != {self.chunk_size}"
                        )
                except Exception as e:
                    print(f"❌ Processing callback error: {e}")
                    import traceback
                    traceback.print_exc()
                    with self._lock:
                        self._chunks_dropped += 1
                    return

            # Update stats
            with self._lock:
                self._chunks_captured += 1

        except Exception as e:
            print(f"❌ Audio capture error: {e}")
            import traceback
            traceback.print_exc()
            with self._lock:
                self._chunks_dropped += 1

    def start(self):
        """Start audio capture (non-blocking)."""
        if self._running:
            raise RuntimeError("Audio capture already running")

        # Use device's native channel count to avoid fallback to different device
        # We'll convert to mono in the callback if needed
        device_channels = self.device_info['max_input_channels'] if self.device_info else 1

        # Create and start stream
        self._stream = sd.InputStream(
            device=self.device_id,
            channels=device_channels,  # Use device's native channels (convert to mono in callback)
            samplerate=self.sample_rate,
            blocksize=self.chunk_size,
            callback=self._capture_callback,
            dtype=np.float32
        )

        self._stream.start()
        self._running = True

        # Verify which device is actually being used
        actual_device = self._stream.device
        devices = sd.query_devices()
        if isinstance(actual_device, tuple):
            actual_device_id = actual_device[0]
        else:
            actual_device_id = actual_device

        actual_device_name = devices[actual_device_id]['name'] if actual_device_id is not None else 'default'
        actual_device_info = devices[actual_device_id] if actual_device_id is not None else {}

        print(f"🎙️  Audio capture started")
        print(f"   Device: {actual_device_name} (id={actual_device_id})")
        print(f"   Channels: {device_channels} (device native: {actual_device_info.get('max_input_channels', 'unknown')})")
        print(f"   Sample Rate: {self.sample_rate}Hz, Chunk Size: {self.chunk_size} samples")

        # Warn if actual device doesn't match selected device
        if actual_device_id != self.device_id:
            print(f"⚠️  WARNING: sounddevice is using device {actual_device_id} ({actual_device_name})")
            print(f"             but we selected device {self.device_id} ({self.device_info['name'] if self.device_info else 'unknown'})")
            print(f"   This usually means the selected device doesn't support the requested parameters.")

    def stop(self):
        """Stop audio capture and cleanup."""
        if not self._running:
            return

        if self._stream:
            self._stream.stop()
            self._stream.close()
            self._stream = None

        self._running = False

        print(f"⏹️  Audio capture stopped "
              f"(captured {self._chunks_captured} chunks, dropped {self._chunks_dropped})")

    @property
    def chunks_captured(self) -> int:
        """Number of chunks successfully captured."""
        with self._lock:
            return self._chunks_captured

    @property
    def chunks_dropped(self) -> int:
        """Number of chunks dropped due to errors."""
        with self._lock:
            return self._chunks_dropped


def list_devices():
    """List available audio input devices (helper function)."""
    print("\n📱 Available Audio Input Devices:")
    devices = sd.query_devices()
    for idx, device in enumerate(devices):
        if device["max_input_channels"] > 0:
            default = " (default)" if idx == sd.default.device[0] else ""
            print(
                f"  [{idx}] {device['name']}{default} "
                f"({device['max_input_channels']} channels, "
                f"{device['default_samplerate']:.0f}Hz)"
            )
