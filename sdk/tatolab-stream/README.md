# tatolab-stream

The Python SDK for [Tatolab](https://tatolab.com), which turns cameras, microphones, screens,
models and your own code into realtime streams that browsers, other streams and AI agents can
read.

> **Looking to get started? Install the Tatolab app first, not this package.**
> `tatolab-stream` is the library you write streams with. It doesn't run them: the Tatolab
> runtime on your machine does, and it comes with the app.
>
> - **macOS** (Apple silicon, macOS 15 or later):
>   `brew install --cask tatolab/tap/tatolab`, then `open -a Tatolab`
> - **Linux** (x86_64 with a Vulkan GPU):
>   `curl -fsSL https://get.tatolab.com/install.sh | sh`
>
> Then follow the [Quickstart](https://tatolab.com/docs/quickstart). `tatolab new` creates a
> project that already depends on this package, so you rarely `pip install` it yourself.

## What it is

A stream is an ordinary Python project. You write nodes as Python classes and connect them in a
function decorated `@stream`:

```python
from tatolab.stream import CameraSource, StreamBuilder, stream

from nodes.halftone_lens_effect import HalftoneLensEffect


@stream
def main(stream_builder: StreamBuilder) -> None:
    camera = stream_builder.add(CameraSource)
    effect = stream_builder.add(HalftoneLensEffect)
    stream_builder.connect(camera.output("video"), effect.input("video_from_upstream"))
    stream_builder.expose(effect.output("video_to_downstream"))
```

`tatolab dev` runs it in the runtime. Each node runs in its own process, so one slow node never
blocks another. An exposed output gets an address other streams, agents and browsers can read.

This package holds the authoring API (`@stream`, `@node`, `StreamBuilder`), typed classes for
every built-in node (camera, test pattern, microphone, speaker, H.264, H.265 and Opus codecs, and
more), and the protocols your nodes are handed at run time. It installs and imports without the runtime,
with complete type hints, so your editor, type checker and unit tests work on any machine.

## Add it to an existing project

```sh
uv add tatolab-stream
```

The runtime runs your nodes with the project's `.venv/bin/python`, so anything you install there
your nodes can import. See [Python SDK](https://tatolab.com/docs/install/python) for details.

Supports CPython 3.10 to 3.13 (not the free-threaded builds).

## Other packages

- [`tatolab-webrtc`](https://pypi.org/project/tatolab-webrtc/): WHIP publish and WHEP play nodes.
- `tatolab` on PyPI is a placeholder that installs nothing. Tatolab itself installs as an app,
  as above.

## Links

- [Documentation](https://tatolab.com/docs)
- [Install Tatolab](https://tatolab.com/docs/install)
- [Write a node](https://tatolab.com/docs/build/nodes)
- [Python API reference](https://tatolab.com/docs/reference/python-api)
- [Source](https://github.com/tatolab/streamlib)

Licensed under the [Business Source License 1.1](https://github.com/tatolab/streamlib/blob/main/LICENSE).
