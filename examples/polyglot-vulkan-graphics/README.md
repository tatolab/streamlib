# polyglot-vulkan-graphics

A Python **or** Deno subprocess processor draws a triangle into a host-allocated
render-target DMA-BUF `VkImage` via `VulkanContext.dispatch_graphics`: the call
routes through escalate IPC (`register_graphics_kernel` + `run_graphics_draw`)
to the host's `VulkanGraphicsKernel::offscreen_render`, and the host reads the
result back via Vulkan to a PNG. The graphics-pipeline half of streamlib's
Vulkan surface-adapter story.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let source = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
    /* config */,
))?;
let graphics = runtime.add_processor(ProcessorSpec::new(
    runtime_kind.processor_ref(),   // processor_type_ref! for the python or deno provider
    graphics_config,
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses
three: the in-repo `@tatolab/debug-utilities` trigger source, plus its own
`./python` and `./deno` polyglot processor packages.

(The host-side setup hook that pre-allocates the render-target surface +
timeline and installs the graphics-kernel bridge is separate application wiring
— it is the adapter integration this example exists to demonstrate, not module
loading.)

## Run it

```bash
./setup.sh                                              # one-time local link
cargo run -- --runtime=python --output=/tmp/vulkan-triangle-py.png
cargo run -- --runtime=deno   --output=/tmp/vulkan-triangle-deno.png
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust, Python, and
   Deno streamlib SDK surfaces at the in-repo checkout (crates.io patch + uv
   source + deno import-map). The SDKs aren't published yet; once they are, the
   by-version pins resolve with no link step.
2. **Packages** — `streamlib link` symlinks `@tatolab/debug-utilities` and this
   example's `./python` + `./deno` packages into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path libs/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
