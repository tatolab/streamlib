# tatolab-ops

The small function nodes every Tatolab stream can wire, built the way a third
party builds a module: `@node.function` functions in an ordinary Python package
that depends on `tatolab-stream` and on no engine crate. Each body is compiled to
native code by Numba when the helper that reads it loads, and runs inside that
helper's read, with no process of its own.

- `Lfo` is a sine wave from -1 to 1 at `rate_hz`, advanced by the stream time
  between reads.
- `Remap` maps `x` linearly from `from_low`..`from_high` (-1..1 by default) onto
  `to_low`..`to_high` (0..1 by default).
- `Clamp` holds `x` between `low` and `high`.

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

Running compiled inside a reader is proven on a runtime, in the integration
suite's `test_helper_placement.py`.
