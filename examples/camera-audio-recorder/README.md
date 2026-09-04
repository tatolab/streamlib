# camera-audio-recorder

Camera + microphone → one MP4, with the recording decoded back into a preview
window. Two tracks, nothing configured between them.

## The model this example teaches

**A track is an inbound link.** Both encoders connect to the sink's one
`tracks` input, and `Mp4Sink` makes one track per link that enters it — named
after the channel that link subscribed to. Two cameras would be two video
tracks and three microphones three audio tracks, with no key to set:

```python
def setup(rt: Runtime) -> None:
    recorder = rt.add(Mp4Sink, config={"path": ...})

    rt.connect(camera.output("video"), video_encoder.input("video"))
    rt.connect(video_encoder.output("encoded_video"), recorder.input("tracks"))
    rt.connect(video_encoder.output("encoded_video"),
               preview_decoder.input("encoded_video"))
    rt.connect(preview_decoder.output("video"), window.input("video"))

    rt.connect(microphone.output("audio"), audio_encoder.input("audio"))
    rt.connect(audio_encoder.output("encoded_audio"), recorder.input("tracks"))
```

- Every processor here is a **native built-in**, statically linked into the
  `streamlib` wheel. `rt.add` takes the class itself — it is a marker, never
  instantiated. No frame and no sample enters a Python interpreter, so a
  recording costs Python nothing per frame.
- **The track's kind comes from the bag, not from configuration.** The sink
  reads each bag's `codec`: `h264`/`h265` makes a video track, `opus` an audio
  track. Swapping `H264Encoder` for `H265Encoder` changes nothing else here.
- **The microphone's format is never spelled in this file.** `OpusEncoder`
  declares a window contract naming a rate, a dtype and a 20 ms window but no
  channel count, so the engine resamples and re-frames to Opus's own clock
  while the channel count follows whatever the device opened at.
- **The preview hangs off the encoder, not the camera.** A channel's one
  publisher shares a single ring config with every subscriber, so a source port
  cannot feed an `ordered` destination and a `newest` one at the same time —
  and `H264Encoder`'s input is `ordered` while `DisplayWindow`'s is `newest`.
  Wiring the camera to both is refused at startup, by name. Both destinations
  of `encoded_video` *are* `ordered`, so that fan-out is legal, and the
  constraint turns into the more honest picture: what reaches the glass is the
  bitstream that reached the file, not the camera frame that preceded it.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib run
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

A GPU with **Vulkan Video encode** queues for H.264. Without them the encoder
logs `the encoder session could not be minted; every later frame is discarded`
and never delivers a sync point, so the sink's header keeps waiting on that
track and the file stays `ftyp`-only — see the header rule under *Observing it*
for how that eventually resolves, and note that on an audio-only hold it
resolves slowly.

The preview additionally needs a **decode** queue; lacking one, the decoder
refuses in `setup()`, sits in state `Error` in `streamlib graph`, and the window
stays black. The recording itself is unaffected — the file is fed by the
encoder, not by the preview.

## Stopping it

**Ctrl-C.** `rt.run()` owns SIGINT while it blocks, so the pipeline stops, each
processor's `teardown()` runs, and the sink closes its open fragment on the way
out. SIGTERM does the same.

The file is **created or truncated at startup**, so re-running overwrites the
last recording rather than appending to it or refusing.

You do not have to stop it cleanly for the file to be usable. The layout is
fragmented — `ftyp`, one `moov`, then a `moof` + `mdat` per fragment — so a
recording plays to its last closed fragment even if the process is killed.
A fragment closes at each of the video track's sync points, which with the
encoder's default 2-second IDR cadence is every 2 seconds.

## Reading what it wrote

`cargo xtask mp4-inspect` from a checkout dumps the container's own metadata as
JSON — no ffprobe:

```bash
cargo xtask mp4-inspect recording.mp4
```

It reports `brands`, one entry under `tracks` per link that reached the sink —
each with its `name`, its `sample_entry` (`avc1` for H.264, `Opus` for the
audio) and its duration — and one entry under `fragments` per closed fragment.
The file also plays in any ordinary player.

A track's `name` is the channel its link subscribed to: the **source
processor's id**, lowercased, joined to its output port — not the display name.
Read it out of `graph` rather than spelling it by hand.

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

The sink writes its `moov` only once every track can be described, because a
sample entry needs the video parameter sets or the Opus header. Until then it
holds every delivered sample, and a link that has produced nothing yet is named
in the log once a second — which is where a silent microphone shows up.

That wait is bounded, but by held bytes rather than by time: when the samples
held for the header pass the writer's 64 MiB budget, the links still silent are
latched by name so the tracks that *did* deliver stop waiting on one that never
will. The budget is sized for several seconds of 1080p, so a stalled video track
resolves quickly — but a hold carrying only Opus fills 64 MiB in hours, so a run
whose video never starts will sit on an `ftyp`-only file for a long time. A
recording that trips the budget has a broken producer, not a busy one.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_RECORDING_PATH` | File to record into. Default `recording.mp4`. |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

The microphone takes the backend's default capture device. Window geometry and
the encoder's settings are literal config in `app.py` — the codec blocks take
more than this app passes them, and their keys are written down in the stubs
(`help(Mp4Sink)` under-reports, so read `streamlib/_engine.pyi`).
