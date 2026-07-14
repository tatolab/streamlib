# polyglot-continuous-processor

A Python **or** Deno subprocess processor running in `execution: continuous`
mode, self-paced by the runner's drift-free `MonotonicTimer`. The scenario
records per-tick timestamps, then asserts the measured inter-tick cadence stays
within tolerance of the manifest's nominal interval — the regression detector
for the runner's monotonic-clock dispatch contract.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `start` with **no
module-loading call** and **no version at the reference site**:

```rust
let processor = runtime.add_processor(ProcessorSpec::new(
    runtime_kind.processor_ref(),   // processor_type_ref! for the python or deno provider
    serde_json::json!({ "output_file": output_file.to_string_lossy() }),
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference. This app uses its
own `./python` and `./deno` polyglot processor packages; the runner picks the
provider by `--runtime`.

## Run it

```bash
./setup.sh                          # one-time local link
cargo run -- --runtime=python
cargo run -- --runtime=deno
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust, Python, and
   Deno streamlib SDK surfaces at the in-repo checkout. The SDKs aren't
   published yet; once they are, the by-version pins resolve with no link step.
2. **Packages** — `streamlib link` symlinks this example's `./python` + `./deno`
   packages into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
