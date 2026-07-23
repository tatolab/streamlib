# tokio-integration

Shows that `Runner::new()` works from inside an existing `#[tokio::main]`
application — it auto-detects the ambient tokio runtime and uses the current
handle instead of trying to create a new one.

A pure API demo: it starts an empty runtime, runs an async operation alongside
it, reads the graph state, and shuts down. There is no processor graph, so no
`add_processor`, no `processor_type_ref!`, and no processor package to load.

## Run it

```bash
./setup.sh        # one-time: link the in-repo SDK
cargo run
```

`./setup.sh` runs `streamlib link --engine <checkout>` so the app's
`streamlib` dependency resolves against the in-repo SDK (a transient
`[patch.crates-io]`; there is no hosted registry — the linked checkout is the
SDK source). Because this
demo loads no processor packages, that is the only setup step. The `streamlib`
CLI must be on your `PATH` (build it with `cargo build -p streamlib-cli` from
the checkout); `setup.sh` falls back to the checkout's built binary otherwise.

`Cargo.lock` is not committed (it pins linked-checkout versions).
