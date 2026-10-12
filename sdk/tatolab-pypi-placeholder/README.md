# tatolab

**This package installs nothing.** The name is held by the Tatolab project so that a guessed
`pip install tatolab` lands here and not on someone else's package.

[Tatolab](https://tatolab.com) turns cameras, microphones, screens, models and your own code
into realtime streams that browsers, other streams and AI agents can read. It isn't installed
with pip. Install the app:

- **macOS** (Apple silicon, macOS 15 or later):
  `brew install --cask tatolab/tap/tatolab`, then `open -a Tatolab`
- **Linux** (x86_64 with a Vulkan GPU):
  `curl -fsSL https://get.tatolab.com/install.sh | sh`

Then follow the [Quickstart](https://tatolab.com/docs/quickstart).

## Tatolab's Python packages

- [`tatolab-stream`](https://pypi.org/project/tatolab-stream/): the Python SDK you write
  streams with. A project made by `tatolab new` already depends on it.
- [`tatolab-webrtc`](https://pypi.org/project/tatolab-webrtc/): WHIP publish and WHEP play
  nodes.

Both need the Tatolab runtime, which comes with the app, to run a stream.

- [Documentation](https://tatolab.com/docs)
- [Source](https://github.com/tatolab/streamlib)
