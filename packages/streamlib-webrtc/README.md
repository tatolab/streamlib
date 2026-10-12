# tatolab-webrtc

WHIP publish and WHEP play for [Tatolab](https://tatolab.com) streams, as an extension wheel:
Rust inside, two ordinary `@node` classes as the binding.

> **New to Tatolab? Install the Tatolab app first.** This package adds two nodes to a stream
> project; the Tatolab runtime, which comes with the app, is what runs them. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-webrtc
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

See [WebRTC (WHIP and WHEP)](https://tatolab.com/docs/build/webrtc) for full examples and how a
session behaves.
