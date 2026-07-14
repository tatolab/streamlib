# camera-deno-subprocess

Camera → a Deno/TypeScript GPU halftone processor → Display. The Deno
subprocess reads camera pixels through shared memory, applies a halftone dot
pattern on the GPU (WebGPU compute via TypeGPU), and forwards the result to the
display.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `connect` + `start`
with **no module-loading call** and **no version at the reference site**:

```rust
let camera = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "camera", "Camera"),   // no version, no load call
    serde_json::json!({}),
))?;
let halftone = runtime.add_processor(ProcessorSpec::new(
    processor_type_ref!("tatolab", "camera-deno-subprocess", "HalftoneProcessor"),
    serde_json::json!({}),
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses
three: the in-repo `@tatolab/camera` + `@tatolab/display` processors, plus its
own `./deno` halftone package.

## Run it

```bash
./setup.sh        # one-time: link the SDK + the packages this app uses
cargo run
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust + Deno
   streamlib SDK surfaces at the in-repo checkout. The SDKs aren't published
   yet; once they are, the by-version pins resolve with no link step.
2. **Packages** — `streamlib link` symlinks `@tatolab/camera`,
   `@tatolab/display`, and this example's `./deno` package into
   `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
