# Next Up

## Dependency Graph

```
#150 Unify processor schema into streamlib.yaml
  │
  │  (eliminate schemas/ duplication — macro reads from
  │   streamlib.yaml by processor name, single source of truth)
  │
  ▼
#135 streamlib-python-native FFI
  │
  │  (gives Python processors direct iceoryx2 shared memory access,
  │   eliminates 6 pipe round-trips per frame)
  │
  ▼
#144 Replace custom pubsub bus with iceoryx2
  │
  │  (architectural — consolidate all IPC onto iceoryx2,
  │   enables cross-process event observability)
  │
  ▼
#143 Remaining advanced .slpkg features
  │
  │  (JTD codegen in pack, streamlib.lock, custom schemas,
  │   namespace, URL loading in runtime API)
```

## Task List

- [x] **#150** — Unify processor schema into `streamlib.yaml`. The macro argument is always a processor name — `#[streamlib::processor("com.tatolab.camera")]` — looked up in `CARGO_MANIFEST_DIR/streamlib.yaml`. No file path support. All standalone YAML files consolidated into per-crate `streamlib.yaml` files. Eliminates `schemas/` directories and makes all Rust processors consistent with Python/TypeScript (single `streamlib.yaml` source of truth). *(PR #151)*

- [ ] **#135** — streamlib-python-native FFI cdylib. Copy the `streamlib-deno-native` pattern to create `streamlib-python-native`. Gives Python subprocess processors direct iceoryx2 shared memory access via FFI, eliminating 6 pipe round-trips per frame (stdin/stdout JSON → direct shared memory read/write).

- [ ] **#144** — Replace custom pubsub bus (`core/pubsub/`) with iceoryx2 Event + Pub/Sub patterns. Consolidate all inter-component communication onto iceoryx2. Runtime events (lifecycle, graph changes, compiler, input) become iceoryx2 services alongside frame data. Enables cross-process event observability (CLI watching events in real-time without HTTP polling). Design-first — see issue for open questions on event serialization, wildcard subscriptions, history/replay, and backpressure.

- [ ] **#143 (remaining)** — Advanced `.slpkg` features not yet implemented: JTD codegen integration in `streamlib pack`, `streamlib.lock` with file checksums, custom `schemas/` section in `ProjectConfig`, `package.namespace` field, URL loading in `runtime.load_package()`. Lower priority polish — pick items as needed.

## Issues

- https://github.com/tatolab/streamlib/issues/135
- https://github.com/tatolab/streamlib/issues/143
- https://github.com/tatolab/streamlib/issues/144
- https://github.com/tatolab/streamlib/issues/150
