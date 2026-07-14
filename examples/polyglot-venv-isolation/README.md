# polyglot-venv-isolation

Proof that each Python streamlib package gets its own per-package virtual
environment. Two example-local packages pin *conflicting* numpy versions
(`pkg-a` → `1.26.4`, `pkg-b` → `2.1.3`) that can never co-resolve in one shared
environment. Each processor reports the numpy version it actually imported; both
holding simultaneously is the isolation proof. GPU-free and hardware-free.

## The model this example teaches

Like every streamlib app, the wiring is `add_processor` + `start` with **no
module-loading call** and **no version at the reference site**:

```rust
let processor = runtime.add_processor(ProcessorSpec::new(
    (pkg.processor_ref)(),   // processor_type_ref! for pkg-a / pkg-b
    serde_json::json!({ "output_file": output_file.to_string_lossy() }),
))?;
```

The referenced packages live in this app's **`streamlib_modules/`** folder and
the runtime lazily discovers + loads each on first reference, provisioning each
package's own venv. This app uses two example-local Python packages,
`./pkg-a/python` and `./pkg-b/python`.

## Run it

```bash
./setup.sh        # one-time local link
cargo run
```

`./setup.sh` does the full local setup in one shot:

1. **SDK** — `streamlib link --engine <checkout>` points the Rust + Python
   streamlib SDK surfaces at the in-repo checkout. The SDKs aren't published
   yet; once they are, the by-version pins resolve with no link step. (numpy
   resolves from public PyPI normally — a truly-external dep.)
2. **Packages** — `streamlib link` symlinks this example's `./pkg-a/python` +
   `./pkg-b/python` packages into `./streamlib_modules/`.

The `streamlib` CLI must be on your `PATH` (`cargo build -p streamlib-cli`, or
`cargo install --path tools/streamlib-cli`); `setup.sh` falls back to the
checkout's built binary.

## What's committed vs generated

`streamlib_modules/`, `streamlib.lock`, `Cargo.lock`, and the
`streamlib link --engine` override are **not committed** — they are regenerated
by `./setup.sh`.
