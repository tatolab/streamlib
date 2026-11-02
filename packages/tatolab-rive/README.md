# tatolab-rive

A Rive animation as a stream source, built the way a third party builds one: a
standalone maturin wheel that depends on `tatolab-stream`, `pyo3`,
`tatolab-module-sdk` and the consumer RHI, and on no engine crate.

- `RiveSource` plays a `.riv` file's state machine in its own processor
  interpreter. On `start()` it lends its `video` output and `data` input to
  this wheel's Rust. A thread there creates a Vulkan device on the runtime's
  GPU through the consumer RHI's external-renderer device, builds
  rive-runtime's Vulkan renderer on it, and per frame draws the artboard into a
  texture from the node's output pool and publishes a `VideoFrame` naming it.
  No frame is copied, and Python never runs at frame cadence.
- Config: `riv_path`, `artboard` and `state_machine` (the file's defaults when
  absent), `width`, `height` and `frame_rate` (1920, 1080 and 60).
- `data` takes msgpack maps from view-model property path to value, applied to
  the artboard's default view-model instance. A path the instance lacks is
  logged once and skipped.

Linux only for now.

## Build it

`build.rs` fetches rive-runtime at a pinned revision and builds it with
rive-runtime's own `build/build_rive.sh`, together with the C++ bridge in
`native/`. It needs `git`, `ninja`, `clang`, `make`, `python3`,
`glslangValidator`, `spirv-opt`, `nm` and `objcopy` on `PATH`, and libuuid's
headers (`uuid-dev`) for premake. Set
`TATOLAB_RIVE_RUNTIME_DIRECTORY` to build against a checkout of your own.

## Run it

With this machine's runtime running, set `RIV_PATH` and `TITLE_PROPERTY_PATH`
in `streams/rive-lower-third/stream.py`, then:

```bash
cd streams/rive-lower-third
tatolab dev
```

The stream exposes H.264 at `http://127.0.0.1:4446/watch/<machine>/main`, and
the source's thread logs its frame rate every 600 frames.

## Check it

```bash
cargo fmt --all --check && cargo test --locked --lib && cargo clippy --locked --lib --all-targets -- -D warnings
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.rive._native && uvx pyright@1.1.411
```

rive-runtime and the C++ projects it builds are under their own licences; see
`THIRD-PARTY-NOTICES.md`.
