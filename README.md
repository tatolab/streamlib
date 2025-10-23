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

# Install npm dependencies (for Nx)
npm install

# Install Python dependencies using uv
uv sync
```

## Requirements

- **Python:** Python 3.10+ with uv package manager
- **Rust:** Latest stable Rust toolchain (for streamlib-core)
- **GPU:** WebGPU-capable GPU (most GPUs since 2016)
- **Node.js:** v18+ (for Nx workspace)

## Project Structure

This is an Nx monorepo containing both Rust and Python libraries:

```
gst-mcp-tools/
├── Cargo.toml              # Rust workspace
├── nx.json                 # Nx configuration
├── package.json            # npm workspace
├── libs/
│   ├── streamlib-core/     # Core Rust library (zero-copy, real-time)
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   └── examples/
│   └── streamlib/          # Python API (user-facing)
│       ├── pyproject.toml
│       ├── src/streamlib/
│       └── tests/
└── examples/               # Standalone example projects
```

## Running Examples

### Rust Examples

```bash
# Run Rust simple example
cargo run --example simple

# Or using Nx
npx nx build streamlib-core
```

### Python Examples

```bash
# Run Python examples with uv
uv run python examples/your_example.py
```

## Development

### Nx Commands (Recommended)

```bash
# Build Rust library
npx nx build streamlib-core

# Run Rust tests
npx nx test streamlib-core

# Run Rust linter (clippy)
npx nx lint streamlib-core

# Run Python tests
npx nx test streamlib

# See all available projects
npx nx show projects

# Build everything
npx nx run-many --target=build --all
```

### Rust Development

```bash
# Build Rust crate
cargo build -p streamlib-core

# Run tests
cargo test -p streamlib-core

# Run example
cargo run --example simple

# Check with clippy
cargo clippy -p streamlib-core
```

### Python Development

```bash
# Add Python dependency
uv add package-name

# Add dev dependency
uv add --dev package-name

# Run tests
uv run pytest libs/streamlib/tests/

# Run with specific Python version
uv run --python 3.11 python script.py
```

## Documentation

- **Python API:** See `libs/streamlib/README.md` for high-level Python API documentation
- **Rust Core:** See `libs/streamlib-core/README.md` for core architecture details

## License

MIT
