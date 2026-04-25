# Migration: `broker` → `surface_share`

Issue [#463](https://github.com/tatolab/streamlib/issues/463) retires the
legacy `broker` / `Broker` vocabulary across the surface-sharing
subsystem. The central `streamlib-broker` daemon was removed in
[#380](https://github.com/tatolab/streamlib/issues/380) — every
`StreamRuntime` now stands up its own `UnixSocketSurfaceService` on
`$XDG_RUNTIME_DIR/streamlib-<runtime_id>.sock`, so the old "broker"
noun no longer matches the architecture.

The rename keeps every ABI-visible name shipping for one release cycle
under a deprecated alias. Polyglot apps pinned to the old FFI symbols
or env var keep working without changes; a deprecation warning fires
where it can.

## What changed

### Internal Rust (no back-compat needed)

| Old | New |
| --- | --- |
| crate `streamlib-broker-client` | `streamlib-surface-client` |
| module `streamlib::linux::surface_broker` | `streamlib::linux::surface_share` |
| `SurfaceBrokerState` | `SurfaceShareState` |
| `BrokerHandle` (polyglot shim) | `SurfaceShareHandle` |
| `BrokerVulkanDevice` (polyglot shim) | `SurfaceShareVulkanDevice` |
| `mod broker_client` (polyglot shim) | `mod surface_client` |
| `connect_to_broker(...)` | `connect_to_surface_share_socket(...)` |
| log prefix `[Runtime broker]` | `[Surface share]` |

These are pure Rust internals — no consumers outside the workspace.

### ABI-committed surface (back-compat retained)

#### Env var

| Old | New |
| --- | --- |
| `STREAMLIB_BROKER_SOCKET` | `STREAMLIB_SURFACE_SOCKET` |

The host (`StreamRuntime` spawn ops) sets **both** for one release
cycle so older Python/Deno SDKs that read `STREAMLIB_BROKER_SOCKET`
keep working. The bundled SDKs prefer `STREAMLIB_SURFACE_SOCKET` and
fall back to the legacy name with a deprecation log line.

#### Python FFI symbols (`libstreamlib_python_native`)

| Old (alias, `#[deprecated]`) | New (canonical) |
| --- | --- |
| `slpn_broker_connect` | `slpn_surface_connect` |
| `slpn_broker_disconnect` | `slpn_surface_disconnect` |
| `slpn_broker_resolve_surface` | `slpn_surface_resolve_surface` |
| `slpn_broker_acquire_surface` | `slpn_surface_acquire_surface` |
| `slpn_broker_unregister_surface` | `slpn_surface_unregister_surface` |

#### Deno FFI symbols (`libstreamlib_deno_native`)

| Old (alias, `#[deprecated]`) | New (canonical) |
| --- | --- |
| `sldn_broker_connect` | `sldn_surface_connect` |
| `sldn_broker_disconnect` | `sldn_surface_disconnect` |
| `sldn_broker_resolve_surface` | `sldn_surface_resolve_surface` |
| `sldn_broker_acquire_surface` | `sldn_surface_acquire_surface` |
| `sldn_broker_unregister_surface` | `sldn_surface_unregister_surface` (Linux only) |

Each alias is a thin `#[unsafe(no_mangle)] pub unsafe extern "C"`
wrapper that delegates to the canonical name. Internal Rust callers
(tests, examples) get a `#[deprecated]` warning; external C/ctypes
callers see the same callable symbol.

## Migration steps

### Python apps using `ctypes` directly

```python
# Old
lib.slpn_broker_connect.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
broker = lib.slpn_broker_connect(socket_path.encode("utf-8"), runtime_id.encode("utf-8"))

# New
lib.slpn_surface_connect.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
handle = lib.slpn_surface_connect(socket_path.encode("utf-8"), runtime_id.encode("utf-8"))
```

The bundled `streamlib` Python package binds both names at load time and
prefers `slpn_surface_*`. If your code uses the package directly, no
change is required.

### Deno apps using `Deno.dlopen` directly

```typescript
// Old
const symbols = {
  sldn_broker_connect: { parameters: ["buffer"], result: "pointer" },
  // ...
} as const;

// New — declare both during the back-compat window
const symbols = {
  sldn_surface_connect: { parameters: ["buffer"], result: "pointer", optional: true },
  sldn_broker_connect:  { parameters: ["buffer"], result: "pointer", optional: true },
  // ...
} as const;

// Pick the canonical name first
const handle = (lib.symbols.sldn_surface_connect ?? lib.symbols.sldn_broker_connect)!(buf);
```

### Apps reading the env var directly

```ts
// Prefer the new name, fall back to the legacy
const socket = Deno.env.get("STREAMLIB_SURFACE_SOCKET")
            ?? Deno.env.get("STREAMLIB_BROKER_SOCKET")
            ?? "";
```

```python
socket = (
    os.environ.get("STREAMLIB_SURFACE_SOCKET")
    or os.environ.get("STREAMLIB_BROKER_SOCKET")
    or ""
)
```

## Removal plan

The `broker` aliases ship for at least one release cycle (the cycle
that contains this rename) so existing apps have time to migrate.

A follow-up issue will:
1. Drop the `slpn_broker_*` / `sldn_broker_*` extern functions.
2. Drop the `STREAMLIB_BROKER_SOCKET` env var read in the bundled
   Python/Deno SDKs and the duplicate write in the spawn ops.
3. Drop the legacy bindings from `libs/streamlib-python` and
   `libs/streamlib-deno`.

If you depend on any of the legacy names, update before that follow-up
ships. Internal Rust callers will see `#[deprecated]` warnings during
the back-compat window — treat those as a checklist for what still
needs migration.
