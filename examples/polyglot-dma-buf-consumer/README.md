# polyglot-dma-buf-consumer

Camera → a Python **or** Deno DMA-BUF consumer → Display. The subprocess
receives camera frames over IPC, imports the host-allocated DMA-BUF via
`ctx.gpu_limited_access.resolve_surface(frame.surface_id)`, reads a probe byte,
then forwards the frame unmodified to the display. The pipeline-level gate for
the polyglot consumer DMA-BUF FD path.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let camera = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "camera", "Camera"),   // no version, no load call
    serde_json::json!({ "device_id": device }),
))?;
let consumer = runtime.add_processor(ProcessorSpec::new(
    runtime_kind.processor_ref(),   // processor_type_ref! for the python or deno provider
    consumer_config,
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses
four: the in-repo `@tatolab/camera` + `@tatolab/display` processors, plus its
own `./python` and `./deno` consumer packages; the runner picks the provider by
`--runtime`.

## Run it

```bash
./setup.sh                          # one-time local link
cargo run -- --runtime=python
cargo run -- --runtime=deno
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust, Python, and
   Deno streamlib SDK surfaces at the in-repo checkout. The linked checkout
   is the SDK package source; there is no central package registry.
2. **Packages** — `streamlib link` symlinks `@tatolab/camera`,
   `@tatolab/display`, and this example's `./python` + `./deno` packages into
   `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
