# polyglot-vulkan-compute

A Python **or** Deno subprocess processor dispatches a Mandelbrot compute
kernel against a host-allocated DMA-BUF `VkImage`; the host reads the result
back and writes a PNG. The polyglot half of streamlib's Vulkan surface-adapter
story.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let source = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
    /* config */,
))?;
let compute = runtime.add_processor(ProcessorSpec::new(
    runtime_kind.processor_ref(),   // processor_type_ref! for the python or deno provider
    compute_config,
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses
three: the in-repo `@tatolab/debug-utilities` trigger source, plus its own
`./python` and `./deno` polyglot processor packages.

(The host-side setup hook that pre-allocates the DMA-BUF surface + timelines and
wires the compute-kernel bridge is separate application wiring — it is the
adapter integration this example exists to demonstrate, not module loading.)

## Run it

```bash
./setup.sh                                              # one-time local link
cargo run -- --runtime=python --output=/tmp/mandelbrot-py.png
cargo run -- --runtime=deno   --output=/tmp/mandelbrot-deno.png
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust, Python, and
   Deno streamlib SDK surfaces at the in-repo checkout (crates.io patch + uv
   source + deno import-map). There is no hosted registry; the linked checkout
   is the SDK source.
2. **Packages** — `streamlib link` symlinks `@tatolab/debug-utilities` and this
   example's `./python` + `./deno` packages into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
