# camera-display

Camera → display. A minimal, canonical StreamLib app.

## The model this example teaches

**A StreamLib app is a normal Python codebase.** One venv, one Python version,
ordinary PyPI dependencies. There is no manifest, no `main()`, and nothing is
downloaded or compiled at runtime:

```python
def setup(rt: Runtime) -> None:
    camera = rt.add(CameraSource, config=camera_configuration)
    window = rt.add(DisplayWindow, config={...})

    rt.connect(camera.output("video"), window.input("video"))
```

- `streamlib run` reads `app.py` from the current directory and calls its
  `setup(rt)` — found by convention, never declared.
- `CameraSource` and `DisplayWindow` are **native built-in processors**,
  statically linked into the `streamlib` wheel. `rt.add` takes the class
  itself. Their per-frame paths never enter a Python interpreter, so there is
  nothing to install and nothing to load.
- This app declares no processor of its own. One that did would live in its own
  module — never in `app.py` — because every Python processor runs in its own
  child interpreter that imports the class by name. `streamlib new` scaffolds
  that shape; see `examples/camera-python-effects` for processors in anger.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib dev
```

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`; the wheel carries the Python API, the engine, and the
`streamlib` CLI, so there is no second artifact to install and no shell setup
step.

Use `streamlib dev` while editing and `streamlib run` otherwise — they launch
identically today; `dev` names the edit loop, which is re-running it.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

A port is read as a track at the machine's local relay once it is exposed. Read
the machine's, the stream's and the node's names out of `tatolab graph` rather
than spelling them by hand: a port is the track `<node>/<port>` of the broadcast
`<machine>/<stream>.hang` at the machine's local relay.

```bash
tatolab expose <stream> <camera> video
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/<stream>.hang \
    <camera>/video --count 5 --out /tmp/camera
```

Each object is one frame's record, and it is metadata, not picture:
`surface_id`, `width`, `height`, `timestamp_ns` and `color_info`, around 250
bytes whatever the resolution. The pixels stay in engine-owned GPU memory that
the surface id names, which means nothing outside the runtime. The records tell
you frames are flowing and what they claim to be; to see the pictures, connect
an `H264Encoder` to the camera, expose its `encoded_video`, and open
`http://127.0.0.1:4446/watch/<machine>/<stream>`.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

Capture size and window geometry are literal config in `app.py` — edit them and
re-run. `CameraSource` also takes `max_width` / `max_height`; `DisplayWindow`
takes `title`, `width`, `height`, and `scaling` (`fit` / `fill` / `stretch`).
