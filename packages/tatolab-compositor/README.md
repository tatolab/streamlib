# tatolab-compositor

Video compositing nodes, built the way a third party builds one: an ordinary
Python package that depends on `tatolab-stream` and on no engine crate. Its GPU
work is one compute kernel built through `create_compute_kernel`, the surface
every node is handed.

- `AlphaOverCompositor` layers `layer_0` to `layer_7` back to front onto
  transparent black, in premultiplied alpha, and publishes the composite on
  `video`. It emits once per `layer_0` frame, carrying that frame's timestamp.
  Every other layer contributes its newest frame, held until a newer one
  arrives; a layer that has never sent a frame is left out.
  - Config: `width` and `height` of the canvas, and `layers`, where entry N
    places `layer_N` with `x`, `y` and `opacity`. A layer with no entry sits at
    0,0 at full opacity. Layers are drawn at their own size, never scaled.
  - Each arriving frame is copied by the engine into a texture the node owns,
    so a held layer never holds its producer's frame. Input frames must be
    single-plane RGBA the engine copy accepts.

A lower third over a camera is three nodes:

```python
from tatolab.compositor import AlphaOverCompositor

compositor = stream_builder.add(
    AlphaOverCompositor,
    config={"width": 1920, "height": 1080, "layers": [{}, {"x": 80, "y": 860}]},
)
stream_builder.connect(camera.output("video"), compositor.input("layer_0"))
stream_builder.connect(lower_third.output("video"), compositor.input("layer_1"))
```

## Run it

`streams/camera-with-two-overlays` is a camera with two test-pattern overlays,
encoded and exposed, with the camera wheel's latency probe after the compositor
reporting the composite's frame rate. With this machine's runtime running and a
camera attached:

```bash
cd streams/camera-with-two-overlays
tatolab dev
```

## Check it

```bash
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream -e . pytest
.venv/bin/pytest tests && uvx pyright@1.1.411
```

The pixel tests run on a runtime, in the integration suite's
`test_alpha_over_compositor.py`.
