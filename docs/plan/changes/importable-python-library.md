# Change: importable-python-library

**Change A of the pivot pair — build the new world beside the old.** The rip-out is
`importable-python-library-ripout.md` (Change B), blocked on this change shipping.
Implements the `[importable-python-library]` DECIDED entries (§Product, §Packages &
extension model, §Processor model, §Graphics, §Media I/O, §Language SDKs,
§Distribution, §Control plane). ADR: `docs/decisions/importable-python-library.md`.
Recon verified 2026-08-02: three read-only agent sweeps (engine media primitives,
GIL/data-plane cost, ecosystem precedent) plus a four-agent audit pass (plan coherence,
rip-out completeness, DX red-team, technical risk) — findings folded in below.

Scale tier: touches the processor model and the Python API's public contract → change
artifact + ADR.

## ADDED

- **The wheel proof + interpreter-lifecycle contract** (the riskiest assumption,
  retired first): a maturin-built abi3 PyO3 wheel; `rt.run()` owns SIGINT while it
  blocks (Ctrl-C returns cleanly, CPython's handler restored) and engine teardown
  strictly precedes interpreter finalization — threads joined, anchored thread states
  released, `atexit`/context-manager guarantee on the exception path. Proven against a
  real `python app.py` harness in headless CI — the arrangement the spike never ran.
- **In-process Python authoring**: `Runtime` via PyO3; `@processor` classes execute
  in-process on the dedicated-thread model; `rt.add(ImportedClass)`; the GIL-release
  contract (every blocking native binding releases the GIL; pixels never cross —
  handles/surface ids only) with a test proving a blocked native call stalls no other
  Python processor. Generated `.pyi` stubs ship in the wheel (IDE autocomplete). A
  single-processor test harness (feed a synthetic frame, assert output — no hardware)
  ships with it; it is also how built-ins are tested.
- **Built-in media blocks**: native V4L2 camera and display processors in the engine
  tree, written against the handle-shaped primitives; the engine absorbs the draw step
  via a "present this surface (fit/fill, color-managed)" composition call. Display is
  greenfield against `vulkan_present_target` (window/event-pump code never lived in the
  engine) — budgeted as a rewrite, not a move. Human-shaped failure messages (no
  camera, no `video` group, no Vulkan ICD, wrong Python) plus a test-pattern fallback
  source so the scaffold demos hardware-free.
- **Zero-CPU-copy exchange surface, DLPack first**: frames/textures expose
  `__dlpack__` (CUDA Array Interface may follow; its default-stream semantics are
  weaker), DMA-BUF export/import, and a CPU numpy view fallback via the cpu-readback
  adapter. Tiled textures reach a linear tensor via one GPU blit into an exportable
  staging buffer — stated honestly, never marketed as copy-free. Sync, lifetime
  (pinned to the Python object; ring-slot reuse gated on consume), and layout owned by
  the engine's export surface. Regression tests: use-after-free, read-before-signal.
- **The dev experience**: `dev`/`run` import `app.py` and execute `setup(rt)` (today's
  run boots an empty graph — this is the missing glue); a bad save prints the
  traceback and keeps the last good pipeline running; the MVP edit loop is re-running
  `dev` (reload-on-save is a later nicety, processor-granular per the plan, never
  module machinery); a dev-mode GIL-hold watchdog warns when a callback holds the GIL
  beyond threshold; ~~`dev` binds loopback~~ — superseded 2026-08-04 by
  `control-plane-bind-posture`: `dev` binds all interfaces, exactly as `run` does.
  `streamlib new` scaffolds `app.py` +
  `pyproject.toml` + `.python-version` (3.12) with working camera → effect → display
  wiring where the effect touches pixels via the exchange surface.
- **Packaging + release channel**: the CLI ships as the wheel's console script; wheels
  publish as repo release artifacts served through a static PEP 503 simple index
  (`pip install streamlib --index-url …` — one stable incantation; identical artifact
  when PyPI arrives after the rename). System libs (Vulkan loader, window system,
  libcuda) are dlopen'd at runtime, never linked; adapter closure excludes skia;
  GIL-enabled CPython builds only.

## MODIFIED

- `sdk/streamlib-python`: authoring surface (`decorators.py`, `processor_context.py`,
  `frame_payload.py`, `gpu_surface.py`, `clock.py`, `log.py`, `schema_ident.py`,
  `_generated_/`) carries over into the wheel's API; the hosting/provisioning half
  (`_processor_registry.py`, `extract_processors.py`, setuptools packaging,
  `streamlib.yaml`, `cgl_context.py`) is Change-B deletion scope.
- `tools/streamlib-cli` `run`/`dev`: gain the `setup(rt)` import-and-execute path and
  drop `RunnerAutoBuild` from the run path (auto-build dies with Change B).
- Kernel primitives exposed to Python as configured blocks (§Graphics DECIDED): shape
  committed here; API design is its own session before any ticket exists.

## Out of scope (this change)

- Everything REMOVED — that is Change B (`importable-python-library-ripout.md`),
  blocked on this change: the plugin ABI, module system, build tools, Deno SDK, CLI
  verb deletion, survivor rewires, api-server relocation, helper-process re-scope,
  companion operating-model PR.
- PyPI publication and the rename; consumer re-authoring (dispositions recorded in
  Change B); audio built-in (backend OPEN); kernel-blocks API design session; TS SDK;
  networking transport.
