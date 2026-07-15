# api-server

A minimal, dependency-free readiness probe for the streamlib runtime's HTTP
command-and-control plane. It connects to a running `streamlib-runtime` and
probes `GET /health` and `GET /api/registry` on `http://127.0.0.1:9000`.

## The model this example teaches

**The api-server is the runtime, not a loadable module.** The control plane —
graph mutation, registry browsing, `/ws/events` streaming — is a *host*: it
drives `RuntimeOperations`, the processor registry, and pubsub, which the plugin
ABI deliberately does not expose. So `streamlib-runtime` statically links the
api-server and serves it in-process; there is no `add_module`, no
`streamlib_modules/`, and no plugin to load. An app reaches the control plane by
running the runtime and hitting its endpoints — which is exactly what this probe
does, over raw HTTP with no dependencies.

For a full REST + WebSocket walk of every control endpoint
(processor/connection CRUD, `ws://127.0.0.1:9000/ws/events`), see the
`api-server-demo` example.

## Run it

```bash
./setup.sh                    # builds streamlib-runtime from the checkout
```

Then, in one terminal, start the runtime (it serves the control plane on
`http://127.0.0.1:9000`):

```bash
<checkout>/target/debug/streamlib-runtime
```

and in this directory run the probe:

```bash
cargo run                     # targets 127.0.0.1:9000
cargo run -- 127.0.0.1:9000   # or an explicit host:port
```

To point setup at a checkout other than the repo this example ships in:

```bash
STREAMLIB_CHECKOUT=/path/to/streamlib ./setup.sh
```

## What's committed vs generated

`Cargo.lock` and `target/` are not committed. The probe has no external
dependencies — it builds with `cargo build` and nothing else.
