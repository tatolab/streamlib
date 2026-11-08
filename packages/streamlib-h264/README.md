# tatolab-h264

H.264 encode and decode for [Tatolab](https://tatolab.com) streams, as an extension wheel: Rust inside, one
ordinary `@node.process` class per node as the binding.

> **New to Tatolab? Install the Tatolab app first.** This package adds nodes to a stream project;
> the Tatolab runtime, which comes with the app, is what runs it. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-h264
```

```python
from tatolab.h264 import H264Decoder, H264Encoder
```

`H264Encoder` takes `video` in and publishes `encoded_video`, one media frame per access unit — a QUIC varint timestamp in microseconds and
then the Annex-B access unit — with a keyframe opening a group and a `VideoEntry` stated on the
port from the stream's SPS.

On Linux it encodes with Vulkan Video on a device its own helper process creates on the runtime's
GPU, so a fault in the video engine ends that one stream and never the runtime's device; frames
are 8-bit RGBA or BGRA, buffer-backed like the camera and test pattern publish or texture-backed
like a GPU effect or a decoder publishes. On macOS it encodes on Apple's media engine through
VideoToolbox: the runtime converts each texture-backed frame on the GPU into an NV12 IOSurface
from the session's own pool, and VideoToolbox codes that. `effort_level` is refused on macOS,
which has no encoder-effort index.

`H264Decoder` takes media frames in that layout on `encoded_video`, entering the stream at a group
start and, after a loss, discarding to the next one. `video` publishes a `VideoFrame` on an RGBA
texture of the node's output pool at the stream's conformance window, with the colour the SPS
signals. On Linux it decodes into an NV12 buffer its helper exports; on macOS VideoToolbox decodes
into an NV12 IOSurface. Either way the runtime converts that into the texture.
