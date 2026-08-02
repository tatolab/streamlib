# Change: importable-python-library

The rip-out and reshape for the SDK-shape pivot. Implements every
`[importable-python-library]` DECIDED entry (§Product, §Packages & extension model,
§Processor model, §Media I/O, §Networking, §Language SDKs, §Distribution, §Control
plane). ADR: `docs/decisions/importable-python-library.md` (carries the owner-confirmed
five-sentence direction). Recon verified 2026-08-02: three read-only agent sweeps
(engine media primitives, GIL/data-plane cost, ecosystem precedent) plus direct
enumeration of the module loader, plugin host, CLI verbs, and workspace members.

Scale tier: touches the plugin ABI and the processor model → change artifact + ADR.

## ADDED

- The streamlib wheel: maturin-built PyO3 package = Python API + CLI + engine,
  statically linked camera/display; abi3 across a small Python version range; released
  as repo-hosted wheel artifacts (PyPI deferred until the project rename).
- In-process Python authoring: `Runtime` exposed to Python via PyO3; `@processor`
  classes execute in-process; `rt.add` accepts the imported class; `rt.run()` blocks
  with the GIL released while the engine (and any OS-mandated main-thread pump) runs.
- Helper-process placement: the engine may place a Python processor in a process
  spawned from `sys.executable` (same interpreter, same venv) over the existing
  iceoryx2 transport; placement is engine policy with a single opt-in, no other
  user-facing surface.
- GIL-release contract in the Python SDK: every native binding that can block releases
  the GIL around the blocking call; frames travel as handles / surface ids, never as
  pixels in Python.
- Built-in media blocks in the engine tree: camera (V4L2), display, audio as native
  processors written against the handle-shaped primitives (DMA-BUF/OPAQUE_FD import,
  present target, texture ring, color resolution, audio clock).
- Engine present-composition call ("present this surface, fit/fill, color-managed")
  absorbing the draw step the old display package drove through recorder/kernel
  handles — the display block becomes window lifecycle + one call per frame.
- `streamlib new` app scaffold: `app.py` with `setup(rt)`, `pyproject.toml`,
  `.python-version` pinned (3.12), working camera → effect → display wiring.

## MODIFIED

- `sdk/streamlib-python`: subprocess SDK → the wheel's Python API surface (authoring
  decorators kept; hosting/provisioning code dropped).
- Subprocess machinery (`spawn_python_native_subprocess_op`, `subprocess_bridge`,
  `subprocess_escalate`, `streamlib-python-native`, `surface_share`,
  `streamlib-surface-client`): retained and re-scoped as the helper-process placement
  path — same-interpreter spawns only; all venv-provisioning and module-resolution
  tendrils removed.
- Adapters: `streamlib-adapter-vulkan` / `-opengl` / `-cuda` / `-skia` /
  `-cpu-readback` cores retained as in-process interop capabilities; their cross-DSO
  `-abi` and `-helpers` crates removed (see REMOVED).
- CLI: `run` / `dev` stay (thin runner, #1699 shape) plus `nodes` / `graph` / `tap` /
  `logs` / `mcp`; ships inside the wheel as a console script. Live-mutation control
  verbs (`submit` / `replace` / `connect` / `remove`) and their MCP tools are removed;
  `dev`'s hot-reload is the edit loop.
- `sdk/streamlib-jtd-codegen`: internal-only (first-party Rust↔Python type parity
  behind the rip-out-cheap seam); no user-facing codegen verb.
- `sdk/vulkan-jpeg`: retained; cdylib-safety constraint dropped.
- `xtask` / CI: lints and gates referencing the ABI, cdylib flavors, or `.slpkg`
  cross-build soundness removed with their subjects.
- `.claude/` rules, agents, and skills tied to dead systems (rule `plugin-boundary`,
  agents `plugin-abi-expert` / `package-source-expert`, skills
  `author-and-submit-processor` / `hot-swap-live-processor`, CLAUDE.md +
  `engine-doctrine` ABI/packages-lag/`streamlib.yaml`-purity lines): updated in a
  dedicated operating-model PR per the flow rule, sequenced with the rip-out.

## REMOVED

Each bullet is a pattern the ship gate verifies is gone from the tree.

- REMOVED: `runtime/streamlib-plugin-abi` (all 22 vtables, PluginAbiObject, layout tests)
- REMOVED: `sdk/streamlib-plugin-sdk`
- REMOVED: `export_plugin!` / `install_host_services` / `HostServices` / `ProcessorVTable`
- REMOVED: `adapters/streamlib-adapter-abi`
- REMOVED: `streamlib-adapter-vulkan-abi` / `streamlib-adapter-vulkan-helpers`
- REMOVED: `streamlib-adapter-opengl-abi`
- REMOVED: `streamlib-adapter-cpu-readback-abi` / `streamlib-adapter-cpu-readback-helpers`
- REMOVED: `streamlib-adapter-cuda-abi` / `streamlib-adapter-cuda-helpers`
- REMOVED: `streamlib-adapter-skia-abi`
- REMOVED: `runtime/streamlib-engine/src/core/plugin/` (host vtable backings,
  `build_fingerprint`, `twin_drift_guard`, load handshake)
- REMOVED: `runtime/streamlib-engine/src/core/runtime/module_loader/` (incl.
  `BuildOrchestrator`, package archive/staging/ledger/acquire)
- REMOVED: `tools/streamlib-build-orchestrator`
- REMOVED: `tools/streamlib-cargo-build`
- REMOVED: `tools/streamlib-pack`
- REMOVED: `tools/streamlib-cross-rustc-fixture`
- REMOVED: `.slpkg`
- REMOVED: `streamlib_modules`
- REMOVED: `tools/streamlib-cli/src/commands/{add,install,link,pkg,build_on_place,generate,schema,setup}.rs`
  (+ `commands/link/`; `setup` is PATH plumbing for the retired standalone binary —
  pip console scripts own PATH)
- REMOVED: `runtime/streamlib-runtime` (standalone binary)
- REMOVED: `sdk/streamlib-deno` / `sdk/streamlib-deno-native`
- REMOVED: `spawn_deno_subprocess_op`
- REMOVED: `sdk/streamlib-processor-extract` (manifest derivation from source)
- REMOVED: manifest/lockfile types in `sdk/streamlib-idents` /
  `sdk/streamlib-processor-schema` (schema-ident types behind the JTD seam survive)
- REMOVED: `packages/test-fixtures-abi-mismatch`
- REMOVED: `docs/architecture/{plugin-abi,package-development-model,package-source,package-staging-layout,runtime-module-materialization,schema-identity-and-packaging,subprocess-rhi-parity,cdylib-reachability,zero-ceremony-authoring}.md`
  (shipped-state docs follow their code out; adapter docs lose their ABI halves)

## Dispositions — deferred re-authoring (recorded, not ticketed)

Old consumers are re-authored, never mechanically ported, in their own planning
sessions and milestones after the wheel exists:

- Camera / display packages → absorbed into the wheel as built-ins (the one
  disposition that IS this change's work).
- mavlink-class packages → plain-Python processor packages (e.g. over pymavlink).
- Hardware/GPU-heavy packages → Python packages with a native wheel inside exposing
  handles only; the Python processor class is the sole exposed surface.
- `escalate` host-side ops → reshaped with the helper-process machinery (engine tree).
- `polyglot-*` and `camera-deno-subprocess` examples → deleted; replaced by normal
  Python app examples.
- MoQ package → rides `runtime/streamlib-moq` (survives); disposition when
  §Networking is scheduled.

## Out of scope

- PyPI publication and the project rename (repo-hosted wheels until then).
- Consumer re-authoring beyond built-ins absorption (own sessions, own milestones).
- Audio backend selection (§Media I/O OPEN — research memo), A/V sync, additional
  execution flavors, networking transport.
- Folding `streamlib-consumer-rhi` back into the RHI (a later simplification; the
  crate split is harmless meanwhile).
