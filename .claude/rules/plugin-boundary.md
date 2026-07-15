---
paths:
  - "runtime/streamlib-plugin-abi/**"
  - "runtime/streamlib-engine/src/core/plugin/**"
  - "sdk/streamlib-sdk/**"
  - "sdk/streamlib-plugin-sdk/**"
  - "sdk/streamlib-macros/**"
---

# Plugin boundary

- **Everything crossing the plugin ABI is `#[repr(C)]` end-to-end, with a byte-layout regression
  test** in `streamlib-plugin-abi`. Never return `Arc<SomeHostInternalType>` from a
  cdylib-callable method — expose it as a PluginAbiObject instead.
- **PluginAbiObject pattern:** a fixed `(handle, vtable)` prefix for clone/drop dispatch, cached POD
  fields read by `&self` getters with no ABI hop, and a `methods_vtable` when the object exposes
  methods beyond POD getters. Refcount accounting runs in host-compiled code via the clone/drop
  slots; the cdylib never touches the host's `Arc`.
- **`make_*_borrow` helpers must populate the cached POD fields** from the inner (the two-step
  dance) — a zeroed borrow produces silent all-zero output. See
  `docs/learnings/cdylib-make-borrow-cached-fields.md`, and add a matching
  `make_borrow_cached_field_regression_tests` case.
- **Never `.escalate(...)` inside a FullAccess lifecycle body** (`setup`, `teardown`, Manual-mode
  `start`/`stop`) — the dispatcher already holds the gate; call `ctx.gpu_full_access().X()`
  directly.
- **Vocabulary — use exactly:** host, plugin, plugin ABI, vtable, PluginAbiObject, handle. Never
  DSO / FFI / COM / shared-object synonyms in prose.
