# camera-display

Camera → H.264, watched in a browser. The minimal, canonical Tatolab camera
stream.

## The model this example teaches

**A Tatolab stream is a normal Python project.** One venv, one Python version,
ordinary PyPI dependencies. There is no manifest, and nothing is downloaded or
compiled at run time:

```python
@stream
def main(stream_builder: StreamBuilder) -> None:
    camera = stream_builder.add(CameraSource, config=camera_config)
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(camera.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
```

- `tatolab dev` reads `stream.py` from the current directory and loads its one
  `@stream` function into this machine's runtime, `tatolabd` — found by
  convention, never declared.
- `CameraSource` and `H264Encoder` are **native built-ins**: they ship inside
  the runtime, and their per-frame paths never enter a Python interpreter.
  `add` takes the class itself, and its `config` is a `TypedDict`
  (`CameraSourceConfig`), so a type checker catches a misspelled key.
- **The runtime opens no windows.** What a stream shows, it exposes: `expose`
  makes the encoder's output a track at this machine's local relay, which a
  browser plays and any other stream or MoQ client reads. `Exposure.PRIVATE`
  keeps it on this machine; `Exposure.PUBLIC` shares it off the machine too.
- This stream declares no node of its own. One that did would live in its own
  module under `nodes/` — never in `stream.py` — because every Python node runs
  in its own child interpreter that imports the class by name. `tatolab new`
  scaffolds that shape; see `examples/camera-python-effects` for nodes in anger.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI — the authoring surface, and the
only Tatolab package a stream's venv holds. The runtime itself is not on pip.

Use `tatolab dev` while editing — it loads the stream again on every saved edit
— and `tatolab run` otherwise. Both keep the stream as long as the command
runs; Ctrl-C unloads it.

Then open the stream in a browser:

```text
http://127.0.0.1:4446/watch/<machine>/main
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links, exposed ports and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

A node is named after its class, lowercased — `camerasource`, `h264encoder` —
unless `add` passes `name=`. An exposed port is the track `<node>/<port>` of the
broadcast `<machine>/<stream>.hang` at the machine's local relay, so any MoQ
client reads it; this repository's `tatolab-read-track` writes a track's objects
to files:

```bash
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    h264encoder/encoded_video --count 30 --out /tmp/camera
```

Each file is one H.264 access unit, so the files concatenated in `objects.jsonl`
order are an H.264 stream ffmpeg plays.

The camera's own `video` port is internal to the stream. `tatolab expose main
camerasource video` makes it a track too, but each of its objects is a frame's
record, not its picture: `surface_id`, `width`, `height`, `timestamp_ns` and
`color_info`, around 250 bytes whatever the resolution. The pixels stay in
engine-owned GPU memory that the surface id names, which means nothing outside
the runtime — that is why the picture is shared through the encoder.

## Knobs

Everything is literal config in `stream.py` — edit it and save while
`tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. `None` picks the first device found. |
| `max_width` / `max_height` | `CameraSourceConfig` | Resolution cap; the negotiated format is clamped to fit. Default 1920×1080. |
| `Exposure.PRIVATE` | `expose` | `Exposure.PUBLIC` shares the track off this machine, at every relay the runtime has joined. |
