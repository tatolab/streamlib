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
  that shape; see `examples/camera-python-display` for processors in anger.

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

`streamlib tap` samples raw bags off one channel. The channel name is the
**source processor's id**, lowercased, joined to its output port — not its
display name — so read it out of `graph` rather than spelling it by hand:

```bash
CHANNEL=$(streamlib graph | python3 -c 'import json,sys; s=json.load(sys.stdin)["links"][0]["source"]; print((s["processor_id"]+"/"+s["port_name"]).lower())')
streamlib tap "$CHANNEL" --count 5
```

`--count` is an upper bound over a short sampling window, so a slower channel
returns fewer bags rather than blocking — the tap never paces the producer.

A video bag is metadata, not picture: `surface_id`, `width`, `height`,
`timestamp_ns` and `color_info`, around 250 bytes whatever the resolution. The
pixels stay in engine-owned GPU memory that the surface id names. Tap tells you
frames are flowing and what they claim to be; to see them, look at the window.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

Capture size and window geometry are literal config in `app.py` — edit them and
re-run. `CameraSource` also takes `max_width` / `max_height`; `DisplayWindow`
takes `title`, `width`, `height`, and `scaling` (`fit` / `fill` / `stretch`).
