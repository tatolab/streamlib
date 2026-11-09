# tatolab-ops

The small nodes every Tatolab stream can wire, built the way a third
party builds a module: `@node.function` functions in an ordinary Python package
that depends on `tatolab-stream` and on no engine crate. Each body is compiled to
native code by Numba when the helper that reads it loads, and runs inside that
helper's read, with no process of its own; a body that does not compile, such as
`HoldFrame`'s, runs as Python there instead.

- Math: `Add`, `Subtract`, `Multiply`, `Divide`, `Power`, `Minimum`, `Maximum`,
  `Absolute` and `Modulo`. A division or remainder by 0, and a power that is no real
  number, answer 0.
- Shaping: `Lerp`, `Clamp`, `Remap` (from -1..1 onto 0..1 by default), `Smoothstep` and
  `Step`.
- Logic: `Compare` (`less`, `equal` and `greater`, within a tolerance), `Switch`, which
  picks between two values by a bool, `Select`, which picks one of four by an index, and
  `Gate`, which hands a message of any family on unread while `open` and nothing while
  closed, so a reactive node behind a closed gate does not run.
- Values and noise: `Constant`, `Random`, which draws afresh for each stream time and the
  same for the same seed and time, and `PerlinNoise` along `x`.
- Time: `Lfo`, a sine wave from -1 to 1 at `rate_hz`; `Timer`, the seconds since its start
  or reset; `Lag`, which follows its input smoothly; `Trigger`, on for the one evaluation its
  input turns on; `Counter`; and `SampleAndHold`.
- `HoldFrame`, which holds the first Image it is handed and hands it on at each read, so
  one decoded or captured frame can feed a stream that keeps running, until `refresh`
  takes a newer one. Its state holds the frame, so it runs as Python in its reader
  rather than compiled.
- `Feedback`, the one node a loop of function nodes closes through.
- `ImageFile`, a class node rather than a function node, publishes the PNG or JPEG
  project asset its `asset` config names by UUID as an Image: once at start, and
  again on each config change.

```python
from tatolab.ops import Lfo, Remap

lfo = stream_builder.add(Lfo)
brightness = stream_builder.add(Remap)
stream_builder.connect(lfo.output("result"), brightness.input("x"))
stream_builder.connect(brightness.output("result"), dimmer.input("brightness"))
```

## Check it

```bash
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream -e . pytest
.venv/bin/pytest tests && uvx pyright@1.1.411
```

With the runtime's wheel installed beside it, the suite also compiles each node and holds its
native code to the Python it was written as.

Running compiled inside a reader is proven on a runtime, in the integration
suite's `test_helper_placement.py`.
