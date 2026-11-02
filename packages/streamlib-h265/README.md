# tatolab-h265

H.265 encode and decode for [Tatolab](https://tatolab.com) streams, as an extension wheel: Rust inside, one
ordinary `@node` class per node as the binding.

> **New to Tatolab? Install the Tatolab app first.** This package adds nodes to a stream project;
> the Tatolab runtime, which comes with the app, is what runs it. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-h265
```

```python
from tatolab.h265 import H265Decoder, H265Encoder
```

`H265Encoder` takes the same config keys and ports as the built-in it will replace: `video` in,
`encoded_video` out, one media frame per access unit — a QUIC varint timestamp in microseconds and
then the Annex-B access unit — with a keyframe opening a group and a `VideoEntry` stated on the
port from the stream's SPS.

It encodes with Vulkan Video on a device its own helper process creates on the runtime's GPU, so a
fault in the video engine ends that one stream and never the runtime's device. Linux only;
frames are buffer-backed `rgba32` surfaces such as the camera and test pattern publish.

`H265Decoder` takes media frames in that layout on `encoded_video`, entering the stream at a group
start and, after a loss, discarding to the next one. `video` publishes a `VideoFrame` on an RGBA
texture of the node's output pool at the stream's conformance window, with the colour the SPS
signals. It decodes into an NV12 buffer its helper exports, and the runtime converts that into the
texture. Linux only, like the encoder.
