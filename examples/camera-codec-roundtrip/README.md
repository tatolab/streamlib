# camera-codec-roundtrip

Camera → hardware encoder → hardware decoder → window. The picture you see has
been through a real H.264 (or H.265) elementary stream on the way to the glass.

## The model this example teaches

**An encoded stream is an ordinary link.** There is no codec mode, no muxer and
no side channel: the encoder publishes on an output port, the decoder reads on
an input port, and `rt.connect` joins them exactly as it joins camera to window
in `examples/camera-display`.

```python
def setup(rt: Runtime) -> None:
    camera = rt.add(CameraSource, config=camera_configuration)
    encoder = rt.add(encoder_marker)
    decoder = rt.add(decoder_marker)
    window = rt.add(DisplayWindow, config={...})

    rt.connect(camera.output("video"), encoder.input("video"))
    rt.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    rt.connect(decoder.output("video"), window.input("video"))
```

- All four are **native built-in processors**, statically linked into the
  `streamlib` wheel. `rt.add` takes the class itself — it is a marker, never
  instantiated. Their per-frame paths never enter a Python interpreter, so the
  bitstream this app produces costs Python nothing per frame.
- Both codec blocks are added bare. Every config key either one takes is
  optional: the encoder sizes its session from the first frame the camera hands
  it and defaults to a 2-second IDR cadence, and the decoder auto-detects its
  decoded-picture-buffer size from the stream's first SPS.
- **The encoder marker and the decoder marker must agree.** `H264Encoder`
  publishes an H.264 elementary stream and `H265Decoder` will refuse it by
  name; `STREAMLIB_CODEC` picks both ends of the pair together, which is why
  the app looks them up as a tuple rather than reading two variables.
- Swapping the codec changes nothing else in this file. That is the point of a
  block: the graph is the same graph.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib dev
```

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`; the wheel carries the Python API, the engine, and the
`streamlib` CLI.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## What the hardware has to have

This app needs a GPU with **Vulkan Video encode and decode queues** for the
codec it is running. The two halves fail differently, and knowing which you are
looking at saves the diagnosis:

- **No decode queue.** The decoder mints its session in `setup()`, so it
  refuses by name before a frame moves: the processor never reaches Running and
  the app raises rather than starting.
- **No encode queue.** The encoder mints lazily, on the first frame. The app
  starts, the failure latches, every later frame is discarded with one `error`
  line in the log — and the window stays black because nothing is ever encoded
  for the decoder to decode.

That asymmetry is today's engine behaviour, described here rather than papered
over. `streamlib logs` is where the encoder's line lands.

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

The interesting channel here is the encoded one. Read its name out of `graph`
rather than spelling it by hand — a channel is the source processor's **id**,
lowercased, joined to its output port:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == "encoded_video":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
')
streamlib tap "$CHANNEL" --count 5
```

An encoded bag is **not** shaped like a video bag. A video bag is metadata —
`surface_id`, `width`, `height`, `timestamp_ns`, `color_info` — naming pixels
that stay in engine-owned GPU memory, and it is around 250 bytes whatever the
resolution. An encoded bag carries its payload inline: `bitstream` is one
Annex-B access unit as raw bytes, beside `codec`, `is_sync_point`,
`group_index`, `sequence_index`, `width`, `height` and `color`. So an encoded
bag's size is the bitstream's size — it moves with the picture and with the
frame type, where a video bag never does. On a static 1080p test pattern the
predicted frames measure a few hundred bytes each and the keyframes between
them tens of kilobytes.

A Python processor reads that bag with the cast built for it:

```python
from streamlib import EncodedVideoFrame

encoded = ctx.inputs.read("encoded_video", into=EncodedVideoFrame)
if encoded is not None and encoded.is_sync_point:
    sink.write(encoded.annex_b_access_unit_bytes)
```

The one place the attribute and the wire key differ is the payload: the bag
spells it `bitstream`, the cast holds it as `annex_b_access_unit_bytes` and
keeps it off the repr, because an access unit that prints in full buries the
assertion it was part of.

`width` and `height` on an encoded bag are the **coded** extent, before the
conformance crop — both codecs pad up to a block size, so a 1080-line stream
reports 1088. The decoder's output is back at the cropped extent, which is what
the window shows.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_CODEC` | `h264` (default) or `h265`. Anything else is refused by name at `setup()`. |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

Window geometry is literal config in `app.py` — edit it and re-run. The codec
blocks take more than this app passes them: the encoder accepts `width`,
`height`, `fps`, `bitrate_bps`, `keyframe_interval_seconds` and `effort_level`,
the decoder `max_width` and `max_height`. Their meanings are written down in
the stubs — `help(H264Encoder)` under-reports, so read
`streamlib/_engine.pyi`.
