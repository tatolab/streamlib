# test-pattern-halftone-lens

The test pattern → a Slang compute kernel → a CPU meter, seen in Tato Studio's viewer. Pixels
stay on the GPU and logic runs on the CPU: the two halves every stream that does real work has.
It needs no camera and no display.

## The stream

`streams/main.graph.json` is the stream: three nodes and the two links between them.

- `TestPatternSource` comes from the **`tatolab-testpattern` module**, a dependency in
  `pyproject.toml`.
- `HalftoneLensEffect`, in `nodes/halftone_lens_effect.py`, is one Slang compute kernel: a lens
  drifts around the frame and turns what it covers into dots. A frame arrives as a pixel buffer,
  which a kernel cannot sample, so each one is copied into a texture first; the kernel writes a
  second texture, which the node publishes with `ImageFrameToWrite.of_texture`. The pixels never
  reach the host.
- `BrightnessMeter`, in `nodes/brightness_meter.py`, reads each frame back through an explicit
  `frame.cpu()` view and logs its mean brightness once a second. It sits on its own process, so
  it never slows the picture — it is the node you replace with your model call.

Each node is its own module, because every Python node runs in its own child interpreter, which
imports the class by the `module:ClassName` the graph document names.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load, building `tatolab-testpattern` from
this checkout as an editable path dependency (`[tool.uv.sources]` in `pyproject.toml`). Then
open `halftonelenseffect`'s `video_to_downstream` in Tato Studio's viewer, and watch
`tatolab logs -f` for the meter's `brightness` records.

## Editing it

The shader is `HALFTONE_LENS_SLANG` in `nodes/halftone_lens_effect.py`. Save it while
`tatolab dev` runs and the stream loads again with the new text compiled. `HalftoneLensDials`
and `HALFTONE_LENS_DIALS = struct.Struct("<fff")` are a layout contract: the push-constant
block's two floats and one float, in that order, so change the two together.

Swap `tatolab.testpattern:TestPatternSource` for `tatolab.camera:CameraSource` (add
`tatolab-camera` to `pyproject.toml`), which publishes on the same `video` port, to run your
camera through the same effect.
