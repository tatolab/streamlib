# streamlib Rust Core

This directory contains the Rust implementation of streamlib's GPU-accelerated video processing runtime.

## Structure

```
streamlib-core/
├── Cargo.toml                 # Workspace root
├── streamlib-core/            # Platform-agnostic core traits
├── streamlib-apple/           # iOS + macOS implementation (Metal)
├── streamlib-vulkan/          # Linux/Jetson implementation (Vulkan)
├── streamlib-py/              # Python bindings (PyO3)
└── streamlib-runtime/         # Standalone binary with A2A + WHIP/WHEP
```

## Building

```bash
# Build all crates
cargo build --workspace

# Build release
cargo build --workspace --release

# Run tests
cargo test --workspace
```

## Development Status

🔨 **IN PROGRESS** - Core architecture being implemented.

See [../../../PLANNING.md](../../../PLANNING.md) for migration roadmap.
