# tatolab-microphone

The microphone node for Tatolab streams.

```python
from tatolab.microphone import MicrophoneSource

microphone = stream_builder.add(MicrophoneSource, config={"device_id": None})
```

`MicrophoneSource` publishes timestamped f32 Channels blocks on `audio`, at
the device's rate and channel count. Each block carries the device's instant of
its first sample.

The node runs in its own helper process. Its native half opens the capture
stream through the engine's audio device backend — PipeWire, else ALSA, on
Linux; CoreAudio on macOS; silence where no backend exists — which the module
host table exports, and a native thread publishes every block with no Python
attached. A stream without this node opens no microphone and never asks for
microphone access.

Built as a third party builds a module: a standalone maturin project depending
on `tatolab-stream` and `tatolab-module-sdk`, never on an engine crate.
