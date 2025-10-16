# streamlib

**Realtime streaming platform where AI agents can easily compose:**

- 📹 Live camera streams
- 🤖 ML models (object detection, segmentation, etc.)
- 🎵 Dynamic audio/video generation
- ✨ Real-time visual effects and overlays
- ⚡ All running on GPU at 60fps

**This is the core vision. Everything else is in service of this goal.**

## Installation

```bash
# Clone repository
git clone https://github.com/tatolab/gst-mcp-tools.git
cd gst-mcp-tools

# Install dependencies using uv
uv sync
```

## Requirements

- Python 3.10+
- WebGPU-capable GPU (most GPUs since 2016)
- Updated GPU drivers

## Running Examples

```bash
# Run examples with uv
uv run python examples/your_example.py

# Run tests
uv run pytest packages/streamlib/tests/
```

## Project Structure

```
gst-mcp-tools/
├── packages/
│   └── streamlib/          # Core streaming SDK
│       ├── src/streamlib/  # Source code
│       ├── tests/          # Test suite
│       └── README.md       # API documentation
├── examples/               # Standalone example projects
└── README.md              # This file (setup instructions)
```

## Documentation

See `packages/streamlib/README.md` for API documentation and usage examples.

## Development

```bash
# Add dependency
uv add package-name

# Add dev dependency
uv add --dev package-name

# Run with specific Python version
uv run --python 3.11 python script.py
```

## License

MIT
