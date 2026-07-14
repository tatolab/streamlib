# polyglot-vulkan-ray-tracing

A Python **or** Deno subprocess processor traces a single-triangle scene into a
host-allocated storage-image `VkImage` via `VulkanContext.dispatch_ray_tracing`:
the processor builds a BLAS + TLAS and dispatches a trace through escalate IPC
(`register_acceleration_structure_blas` / `_tlas`, `register_ray_tracing_kernel`,
`run_ray_tracing_kernel`) to the host's `VulkanRayTracingKernel::trace_rays`, and
the host reads the storage image back to a PNG. The ray-tracing half of
streamlib's Vulkan surface-adapter story.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let source = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
    /* config */,
))?;
let rt = runtime.add_processor(ProcessorSpec::new(
    runtime_kind.processor_ref(),   // processor_type_ref! for the python or deno provider
    rt_config,
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses
three: the in-repo `@tatolab/debug-utilities` trigger source, plus its own
`./python` and `./deno` polyglot processor packages.

(The host-side setup hook that pre-allocates the storage-image texture +
timeline and installs the ray-tracing-kernel bridge is separate application
wiring — it is the adapter integration this example exists to demonstrate, not
module loading.)

## Run it

```bash
./setup.sh                                              # one-time local link
cargo run -- --runtime=python --output=/tmp/vulkan-rt-py.png
cargo run -- --runtime=deno   --output=/tmp/vulkan-rt-deno.png
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust, Python, and
   Deno streamlib SDK surfaces at the in-repo checkout (crates.io patch + uv
   source + deno import-map). The SDKs aren't published yet; once they are, the
   by-version pins resolve with no link step.
2. **Packages** — `streamlib link` symlinks `@tatolab/debug-utilities` and this
   example's `./python` + `./deno` packages into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
