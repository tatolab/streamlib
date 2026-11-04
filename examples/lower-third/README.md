# lower-third

A live lower third and a Rive station bug over the camera, at 1080p60, where a
data message swaps the template without restarting the stream.

```
CameraSource ──────────────────────────────────────────────▶ layer_0 ─┐
LowerThirdRundown ─ data ─▶ BrowserSource (lower_third.html) ─▶ layer_1 ─┼─ AlphaOverCompositor ─ H264Encoder
RiveSource (station_bug.riv) ───────────────────────────────▶ layer_2 ─┘
```

- `templates/lower_third.html` is the lower third: an ordinary web page on a
  transparent background, rendered by `tatolab-browser`. It listens for
  `tatolab:data` and restyles itself from the event's `detail`.
- `LowerThirdRundown` (`src/lower_third/nodes/`) is the data. Every few seconds
  it writes the next `LowerThirdTemplate` (`variant`, `title`, `subtitle`) to
  the page's `data` input. Point any node, or anything that writes to that
  port, at the same input to drive the graphic from your own data.
- `templates/station_bug.riv` plays its state machine in `tatolab-rive`.
- `AlphaOverCompositor` stacks the three, and the H.264 encoder exposes the
  result. The runtime opens no windows; what a stream shows, it exposes.

Each node runs in its own child process, with its own interpreter.

## Run it

With this machine's runtime running and a camera attached:

```bash
uv sync
tatolab dev
```

Watch it at `http://127.0.0.1:4446/watch/<machine>/main`. Set
`CAMERA_DEVICE_ID` in `stream.py` to pick a camera.

To make the graphic your own, change the page and the `LOWER_THIRD_VARIANTS`
it styles together, and send your own `LowerThirdTemplate` records.

## Check it

```bash
uv run --group dev pytest
```

`templates/station_bug.riv` is rive-runtime's `coin.riv` test asset, under
rive-runtime's MIT licence; see `THIRD-PARTY-NOTICES.md`.
