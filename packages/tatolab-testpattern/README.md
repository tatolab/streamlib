# tatolab-testpattern

The sample stream's test pattern, built the way a third party builds a node: an
ordinary Python package that depends on `tatolab-stream` and on no engine crate.
Its GPU work is one compute kernel built through `create_compute_kernel`, the
surface every node is handed.

- `TestPatternSource` publishes SMPTE-style colour bars with a bouncing potato
  and the sample stream's caption panel on `video`, about 30 frames a second, as
  RGBA `VideoFrame`s tagged BT.709 primaries, the sRGB transfer and full range.
  - Config: `width` and `height` of each frame, 1280x720 by default, each side 1
    to 16384 pixels.
  - A frame's picture is a function of its index alone: the potato leaves the
    top-left corner at frame zero, travels four pixels a frame along each axis
    and bounces off every edge; the caption panel sits 24 pixels in from the
    bottom-right corner. The index advances only when a frame is published, so
    a frame the output pool refused never shifts the next one's picture.

```python
from tatolab.testpattern import TestPatternSource

source = stream_builder.add(TestPatternSource, config={"width": 1920, "height": 1080})
```

## Check it

```bash
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream -e . pytest
.venv/bin/pytest tests && uvx pyright@1.1.411
```

The pixel tests run on a runtime, in the integration suite's
`test_native_builtin_blocks.py`.
