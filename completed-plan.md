# Completed Work

## Dependency Graph (Completed)

```
#131 Hash-based venv caching ✅
  │
  ▼
#136 Unified .slpkg package format ✅ (PR #141)
  │
  │  Phase 1: streamlib.yaml manifest + runtime.load_package() ✅
  │  Phase 2: streamlib pack CLI ✅
  │  Phase 3: load_project()/load_package() split + .slpkg extraction ✅
  │  Phase 4: Schema registry, pkg CLI, cross-package dependencies ✅
  │
  ▼
Delete camera-dylib-display + Rust dylib via manifest (#143) ✅
  │
  │  (enables Rust processor plugins to use the same packaging
  │   as Python/TypeScript — streamlib.yaml + .slpkg)
  │
  ▼
camera-rust-plugin example ✅
  │
  │  (simple Camera → Rust dylib processor → Display pipeline,
  │   equivalent of camera-python-subprocess but for Rust plugins)
```

## Task List

- [x] **#131** — Hash-based venv caching. Key venvs on hash of pyproject.toml instead of runtime_id.
- [x] **#136 Phase 1** — `streamlib.yaml` manifest. `runtime.load_package()` for local directories. Replaced `register_python_project()` and `register_deno_project()`.
- [x] **#136 Phase 1.5** — Eager venv creation in `load_package()`. Double-checked locking to serialize concurrent venv creation.
- [x] **#136 Phase 2** — `streamlib pack` CLI. Creates `.slpkg` ZIP bundles from package directories.
- [x] **#136 Phase 3** — `load_project()`/`load_package()` split. `.slpkg` extraction to `~/.streamlib/cache/packages/`. `project_path` baked into constructors.
- [x] **#136 Phase 4** — Schema registry on `ProcessorInstanceFactory`. Embedded JTD schemas (20+). `schemas list/describe/validate-processor` CLI. `pkg install/inspect/list/remove` CLI. Cross-package `dependencies` in `streamlib.yaml`. `streamlib_version` compatibility check. `config_schema` wiring in `load_project()`. CLI separated into `streamlib-runtime` binary (kubectl model).
- [x] **Delete `camera-dylib-display`** — Remove the disabled example from `examples/camera-dylib-display/` (including `rust/` subdirectory and `python/` subdirectory). It uses the old PyO3 in-process Python hosting that was replaced by subprocess architecture. Currently commented out in workspace `Cargo.toml`. Clean removal, no replacement needed at this step. *(PR #146)*
- [x] **Rust dylib loading via `streamlib.yaml`** — Pull forward from #143. Currently `load_project()` rejects `runtime: rust` with an error. Change it to load a compiled dylib from the package's `lib/` directory using the existing `PluginLoader` infrastructure.

  **What exists already:**
  - `streamlib-plugin-abi` crate: `PluginDeclaration` struct, `export_plugin!` macro, ABI version check
  - `PluginLoader` in `streamlib-runtime/src/main.rs`: loads dylibs via `libloading`, finds `STREAMLIB_PLUGIN` symbol, calls `(decl.register)(&PROCESSOR_REGISTRY)`, keeps libraries alive
  - `streamlib run --plugin <path>` and `--plugin-dir <dir>` CLI flags already work
  - Rust plugin processors use the same `#[streamlib::processor()]` macro, just compiled as `cdylib`

  **What changed:**
  1. In `runtime.rs` `load_project()`: replace the `ProcessorLanguage::Rust => return Err(...)` branch with logic that:
     - Looks for a dylib in `{project_path}/lib/` matching platform extension (`.dylib` on macOS, `.so` on Linux, `.dll` on Windows)
     - Uses `PluginLoader` to load it (same flow as `--plugin` flag)
     - The plugin's `export_plugin!` macro handles registration into `PROCESSOR_REGISTRY` — no need for `register_dynamic()`
  2. `PluginLoader` currently lives in `streamlib-runtime`. It may need to move to `streamlib` core (or be duplicated) so `load_project()` can call it. Alternatively, `load_project()` can use `libloading` directly with the same symbol lookup pattern.
  3. In `streamlib pack`: when `runtime: rust` is detected, include `lib/*.dylib` (or `.so`/`.dll`) in the `.slpkg` archive
  4. The `streamlib.yaml` for a Rust plugin package would look like:
     ```yaml
     package:
       name: com.tatolab.simple-effects
       version: "1.0.0"
       description: "Simple video effects as Rust plugin"

     processors:
       - name: com.tatolab.grayscale_rust
         version: "1.0.0"
         description: "Grayscale effect (Rust dylib)"
         runtime: rust
         execution: reactive
         inputs:
           - name: video_in
             schema: com.tatolab.videoframe@1.0.0
         outputs:
           - name: video_out
             schema: com.tatolab.videoframe@1.0.0
     ```
  5. The processor names in `streamlib.yaml` must match the names in `#[streamlib::processor(name = "...")]` inside the dylib — the YAML is declarative metadata, the dylib handles actual registration via `export_plugin!`.

- [x] **Create `camera-rust-plugin` example** — Simple equivalent of `camera-python-subprocess` but with a Rust processor loaded as a dylib. Demonstrates the full packaging flow: compile plugin → `streamlib pack` → `streamlib run --plugin` or `load_package()`. *(PR #148)*

  **Structure:**
  ```
  examples/camera-rust-plugin/
  ├── Cargo.toml              # Main example binary
  ├── src/main.rs             # Camera → RustEffect → Display pipeline
  ├── plugin/
  │   ├── Cargo.toml          # cdylib crate
  │   ├── src/lib.rs          # export_plugin!(GrayscaleProcessor::Processor)
  │   └── streamlib.yaml      # Package manifest (runtime: rust)
  ```

  **Pipeline:** Camera → Grayscale (Rust dylib) → Display. The grayscale processor should be trivial — just demonstrate the loading/packaging, not complex GPU effects.
