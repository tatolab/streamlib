# tatolab-speaker

The speaker node for Tatolab streams.

```python
from tatolab.speaker import SpeakerSink

speaker = stream_builder.add(SpeakerSink, config={"device_id": None})
stream_builder.connect(microphone.output("audio"), speaker.input("audio"))
```

`SpeakerSink` plays the blocks it reads on `audio`, as messages
`tatolab.stream.audio_block.AudioBlock` reads, at any rate, channel count and
dtype: the engine converts every block to 48 kHz stereo `f32` on the way in,
and the audio backend plays that on whatever format the device opened at. A
period the graph had no samples for is played as silence and counted.

The node runs in its own helper process. Its native half opens the playback
stream through the engine's audio device backend — PipeWire, else ALSA, on
Linux; CoreAudio on macOS; discarding where no backend exists — which the
module host table exports, and a native thread feeds the device with no Python
attached.

Built as a third party builds a module: a standalone maturin project depending
on `tatolab-stream` and `tatolab-module-sdk`, never on an engine crate.
