# tatolab-webrtc

WHIP publish and WHEP play for Tatolab streams, as an extension wheel: Rust inside, two
ordinary `@node` classes as the binding.

```bash
pip install tatolab-webrtc
```

It installs from PyPI with the `tatolab-stream` package it depends on.

```python
from tatolab.webrtc import WhepPlayer, WhipPublisher
```

`WhipPublisher` takes one fan-in input, `tracks` — each inbound link becomes one RTP track,
video or audio by the bag's `codec`. `WhepPlayer` emits `encoded_video` and `encoded_audio`,
filling every key of the wire contract from the stream itself.

Both sit on the encoded side of the codec blocks: `H264Encoder` / `OpusEncoder` upstream of
the publisher, `H264Decoder` / `OpusDecoder` downstream of the player.
