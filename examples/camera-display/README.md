# camera-display

The camera, seen in Tato Studio's viewer. The minimal, canonical Tatolab camera
stream.

## The model this example teaches

**A Tatolab stream is a graph asset in a project.** The project is this folder:
`tatolab-project.json` names it, `pyproject.toml` holds its Python
dependencies, and `streams/main.graph.json` is its one stream, `main`. The
graph document is the stream's source of truth — nodes by UUID, their config,
and the links between their ports — and the runtime is its one writer:

```json
{
  "nodes": [
    {"id": "…", "name": "camerasource", "type": "tatolab.camera:CameraSource", "config": {}}
  ],
  "links": []
}
```

- `tatolab dev` queues a load of the project: the runtime reads every graph
  document under `streams/` and loads each as its own stream.
- `CameraSource` comes from the **`tatolab-camera` module**, a dependency in
  `pyproject.toml`. It runs in its own child process, and its per-frame path is
  the module's Rust, never Python. Its config keys are the fields of
  `CameraSourceConfig` in `tatolab.camera`.
- **The runtime opens no windows.** Any port of a running stream is seen in
  Tato Studio's viewer; a stream that sends its picture off the machine adds
  gateway nodes.
- This stream declares no node of its own. One that did would live in its own
  module under `nodes/`, because every Python node runs in its own child
  interpreter that imports the class by name. `tatolab new` scaffolds that
  shape; see `examples/camera-python-effects` for nodes in anger.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The first load makes the project's `.venv` from `pyproject.toml`, which builds
`tatolab-camera` from this checkout as an editable path dependency
(`[tool.uv.sources]`); the module is a maturin project, so the build needs a
Rust toolchain. `tatolab dev` follows the project's log and loads the project
again on every saved edit; `tatolab run` queues the load and returns.

Then watch `camerasource`'s `video` in Tato Studio's viewer.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The camera's `video` port is an `Image` port, and a frame on it names its
picture rather than carrying it: `surface_id`, `width`, `height`, `format`,
`colour` and `timestamp_ns`. The pixels
stay in engine-owned GPU memory that the surface id names.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `device_id` | `camerasource` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. Absent picks the first device found. |
| `max_width` / `max_height` | `camerasource` | Resolution cap; the negotiated format is clamped to fit. Default 1920×1080. |
