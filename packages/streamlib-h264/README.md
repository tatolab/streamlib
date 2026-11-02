# tatolab-h264

H.264 encode for [Tatolab](https://tatolab.com) streams, as an extension wheel: Rust inside, one
ordinary `@node` class as the binding.

> **New to Tatolab? Install the Tatolab app first.** This package adds a node to a stream project;
> the Tatolab runtime, which comes with the app, is what runs it. See
> [Install Tatolab](https://tatolab.com/docs/install) and the
> [Quickstart](https://tatolab.com/docs/quickstart).

In a stream project:

```bash
uv add tatolab-h264
```

```python
from tatolab.h264 import H264Encoder
```

`H264Encoder` takes the same config keys and ports as the built-in it will replace: `video` in,
`encoded_video` out, one media frame per access unit — a QUIC varint timestamp in microseconds and
then the Annex-B access unit — with a keyframe opening a group and a `VideoEntry` stated on the
port from the stream's SPS.

It encodes with Vulkan Video on a device its own helper process creates on the runtime's GPU, so a
fault in the video engine ends that one stream and never the runtime's device. Linux only;
frames are buffer-backed `rgba32` surfaces such as the camera and test pattern publish.
