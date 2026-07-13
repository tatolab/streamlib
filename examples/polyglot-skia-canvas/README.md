# polyglot-skia-canvas

A Python subprocess processor draws an animated 60fps Skia scene into a
host-allocated render-target DMA-BUF `VkImage`: the host registers the surface +
a timeline semaphore via surface-share, the processor opens it through
`SkiaContext.acquire_write` (which imports the DMA-BUF as a `GL_TEXTURE_2D` and
yields a `skia.Surface`), and the host reads frames back via Vulkan to PNGs. The
polyglot half of streamlib's Skia surface-adapter story. Python is the only
runtime today — `skia-python` wraps the Skia C API.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let source = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "debug-utilities", "BgraFileSource"),
    /* config */,
))?;
let canvas = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "polyglot-skia-canvas", "SkiaCanvas"),
    canvas_config,
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses two:
the in-repo `@tatolab/debug-utilities` trigger source, plus its own `./python`
polyglot processor package.

(The host-side setup hook that pre-allocates the render-target surface + timeline
and registers them with surface-share is separate application wiring — it is the
adapter integration this example exists to demonstrate, not module loading.)

## Run it

```bash
./setup.sh                                  # one-time local link
cargo run -- --output-dir=/tmp/skia-canvas-py
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust and Python
   streamlib SDK surfaces at the in-repo checkout (crates.io patch + uv source).
   The SDKs aren't published yet; once they are, the by-version pins resolve with
   no link step.
2. **Packages** — `streamlib link` symlinks `@tatolab/debug-utilities` and this
   example's `./python` package into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path libs/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
