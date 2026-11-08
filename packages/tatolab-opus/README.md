# tatolab-opus

Opus encode and decode for [Tatolab](https://tatolab.com) streams, as an extension wheel:
libopus and the codec state in Rust inside, two ordinary `@node.process` classes as the binding.

> **New to Tatolab? Install the Tatolab app first.** This package adds two nodes to a stream
> project; the Tatolab runtime, which comes with the app, is what runs them. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-opus
```

```python
from tatolab.opus import OpusDecoder, OpusEncoder
```

`OpusEncoder` takes Channels blocks on `audio`, which declares a 20 ms window at 48 kHz in `f32` and
follows the source's channel count (one to eight), and writes one Packets message per Opus packet
on `encoded_audio`, codec `opus`, in hang's legacy container, a QUIC varint timestamp in microseconds and then
the packet. The port's stream facts carry the channel count and the `OpusHead`. Its config
is optional: `bitrate_bps`, and `application` (`audio`, `voip` or `lowdelay`).

`OpusDecoder` takes those Packets on `encoded_audio`, which requires codec `opus`, and writes
planar `f32` Channels blocks on `audio`, at 48 kHz in the stream's own channel count.

Each node runs in its own processor interpreter, and libopus runs in that process.
