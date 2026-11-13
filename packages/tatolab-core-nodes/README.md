# tatolab-core-nodes

The nodes every Tato Studio project is meant to start with, as a project you open in Tato
Studio: add its nodes to a graph, play its streams, and use its views.

The runtime carries it and links it into every project as the import `imports/core-nodes`.

## Open it

In Tato Studio, open this folder as a project. With the runtime running, from the root of this
repository:

```bash
cd packages/tatolab-core-nodes
tatolab dev
```

## What it holds

`core_nodes/` is one module per group the Add node search lists. The package is not named `nodes`, since one interpreter holds one module per top-level name and every project that uses these nodes may hold a `nodes/` of its own.

| Group | Module | Nodes |
| --- | --- | --- |
| Generators | `solid_color.py`, `generators.py` | Solid Color, Gradient, Radial Gradient, Noise, Checkerboard, Stripes, Circle, Rectangle |
| Sources | `image_file.py` | Image File |
| Color | `color.py` | Level, Hsv Adjust, Invert, Monochrome, Threshold, Tint, Duotone, Posterize, Lookup |
| Filters | `filters.py` | Blur, Sharpen, Edge Detect, Pixelate, Displace, Rgb Split, Vignette, Chroma Key, Luma Key |
| Transform | `transform.py` | Transform, Crop, Flip, Tile, Mirror, Kaleidoscope |
| Composite | `composite.py` | Mix, Over, Blend Add, Blend Multiply, Blend Screen, Blend Difference, Mask, Wipe |
| Math | `math_nodes.py` | Add, Subtract, Multiply, Divide, Power, Minimum, Maximum, Absolute, Modulo, Lerp, Clamp, Remap, Smoothstep, Step, Constant, Random, Perlin Noise |
| Animation | `animation.py` | Lfo, Timer, Lag, Ramp, Pulse, Speed, Envelope |
| Logic | `logic.py` | Compare, Switch, Select, Gate, Trigger, Counter, Sample And Hold, Hold Frame |
| Audio | `oscillator.py` | Oscillator |

Every Color, Filters, Transform and Composite node, and every generator but Solid Color, is a
`@node.shader`: Slang the runtime compiles and runs on the GPU, each setting a number input a
number node can drive.

`streams/` holds two streams:

- `main`: drifting noise folded into a kaleidoscope that cycles through colours, with a tone,
  into Program.
- `spotlight`: a circle swinging over a gradient.

`views/program/` is the Program view: a node with a `video` input, drawn full frame, and an
`audio` input, drawn as one level meter per channel.

## What a shader node cannot do yet

- **A generator takes its size from a picture.** A `@node.shader` takes at least one Image, so
  Gradient, Noise and the rest read the size of what is wired into `size_from` and none of its
  pixels. Wire a Solid Color into it.
- **A shader runs once per picture it is sent.** A number changing does not run it, so the
  picture source sets the frame rate: Solid Color sends its picture about 60 times a second.
- **A shader is not told the picture's size.** Circles, rotations and blur radii assume a 16:9
  frame and stretch on any other shape.
- **A shader is not handed the time.** Wire a Timer, Ramp or Speed into the input that moves it.

## Loops

A loop of number nodes closes through Previous Value, a node the runtime ships under Logic. Wire
a number node's output back into a node that feeds it and the runtime adds a Previous Value on
that wire; you can also place one yourself. It hands on the number it was given at the last
evaluation, starting at 0, so Add with 1 on its other input counts up through one.

## Check it

```bash
cd packages/tatolab-core-nodes
uv venv --python 3.12 .venv
VIRTUAL_ENV="$PWD/.venv" uv pip install pytest numpy pillow -e ../../sdk/tatolab-stream
.venv/bin/python -m pytest tests -v
```

The suite checks each node's group, description and Slang signature, and each stream against
the nodes and ports the project brings. It compiles no Slang and runs no stream: that takes a
runtime, and `tests/stream-on-runtime/test_core_nodes_project.py` does it, starting each stream
here and one of each shader node on `tatolabd`.
