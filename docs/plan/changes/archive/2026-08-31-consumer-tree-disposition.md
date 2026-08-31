# consumer-tree-disposition

The tree work that implements §Consumers — examples & packages (decided 2026-08-30,
align PR #2050; rationale in `docs/decisions/consumer-tree-disposition.md`): the
sixteen-entry delete sweep, the engine-tree narration that still describes the
pre-pivot consumer doctrine, the example-coupled CI test that dies with its example,
and the conversion backlog made visible in the tracker.
No conversion is performed here — every conversion is its own later work under the
from-scratch doctrine — and held consumers are untouched by decision: each deletes
through its own domain's future align.

**Scale gate — a decided contract executed against the tree, this skill, no ADR.**
The ADR already exists, written by the align (`docs/decisions/consumer-tree-disposition.md`).
Nothing here touches the RHI, the IPC wire, the processor model, or the Python API's
public contract — packages consume that surface, they do not change it.

**Precondition.** §Consumers (`ARCHITECTURE.md:209`) is six DECIDED bullets and zero
OPEN entries, every one citing `[consumer-tree-disposition]`. Re-verified against the
tree 2026-08-30: 28 example dirs and 15 package dirs exist; `packages/test-fixtures`
is the sole workspace member under either tree (`Cargo.toml:42`); no CI workflow
references either tree; two examples (`camera-display`, `camera-python-effects`)
already wear the converted shape.

---

## REMOVED: the retired sixteen

§Consumers' one-sweep bullet, executed. The ship gate proves each bullet two ways:
the path check (`git ls-files`) proves the directory gone; the content sweep proves
no engine-tree reference survives — `examples/**` and consumer `packages/**` are
sweep-excluded by design, so the residue enumerated on the continuation lines below
is the entire engine-side reference surface, found by running the gate's own search
2026-08-30. Deleting a directory deletes its tracked files; untracked leftovers
(build `target/` junk) are removed in the same commit's working tree.

- REMOVED: examples/pipelines
- REMOVED: examples/camera-deno-subprocess
  Already 0 tracked files — its sources died with the Deno SDK deletion (#1799,
  `2728d765`); only an untracked `target/` remains on disk. The halftone effect is
  mined for the backlog first: `git show
  2728d765^:examples/camera-deno-subprocess/deno/processors/halftone_processor.ts`.
- REMOVED: examples/camera-python-subprocess
- REMOVED: examples/polyglot-manual-source
- REMOVED: examples/camera-rust-plugin
- REMOVED: examples/vulkan-video-roundtrip-cdylib-camera
- REMOVED: examples/dynamic-reconfigure
- REMOVED: examples/api-server
- REMOVED: examples/api-server-demo
- REMOVED: examples/runtime-graph-json-demo
- REMOVED: examples/hello-streamlib
  Engine-side residue: `sdk/streamlib-sdk/src/hello_streamlib_example_e2e.rs`
  `#[path]`-includes the example's source (`:30`). The test deletes with the
  example — see the next section.
- REMOVED: packages/audio
  Referenced only by `docs/decisions/audio-subsystem.md` — a decisions doc,
  sweep-excluded by the annotate-don't-overwrite policy; no edit.
- REMOVED: packages/camera
  Engine-side residue, all stale pre-pivot anchors: doc comments at
  `runtime/streamlib-engine/src/core/rhi/color_converter.rs:8` and
  `runtime/streamlib-engine/src/core/rhi/tone_mapper.rs:19` cite
  `packages/camera/...` shader and processor paths, and
  `docs/architecture/adapter-runtime-integration.md:388` plus
  `docs/architecture/texture-registration.md:495` cite the same dead processor
  path. Re-anchored to the current built-in sources (in-engine since #1709; the
  color-convert shaders live with `vulkan/rhi/vulkan_color_converter.rs`), with
  exact anchors resolved in the ticket.
- REMOVED: packages/display
  Engine-side residue: `docs/architecture/texture-registration.md:488` cites the
  dead `packages/display/processors/display_linux.rs`. Re-anchored as above.
- REMOVED: packages/frame-tap
- REMOVED: packages/core
  An empty untracked stub directory — 0 tracked files; the deletion is an `rmdir`.
  Engine-side residue: the `xtask/src/check_clock_usage.rs:38-48` doc comment names
  it (and the long-deleted `packages/escalate`) as live schema holders — rewritten
  in the MODIFIED section below.

## REMOVED: the example-coupled E2E, deleted with its subject

`sdk/streamlib-sdk/src/hello_streamlib_example_e2e.rs` `#[path]`-includes the
example's actual `hello_forward.rs` (`:30`), drives a fixture frame through it,
and `example_dir_has_no_ceremony_files` (`:207`) walks the example directory.
Owner ruling, 2026-08-30, revising this proposal's first draft (which relocated
the fixture in-crate): the test deletes, on two grounds.

- Redundancy. The zero-ceremony guarantee is proven where it lives — `streamlib
  new` scaffolds a working stack and the CLI tests verify it (§Product's verify
  anchors, `ARCHITECTURE.md:39-40`) — and "does a processor compile and forward a
  frame" is what the rest of the suite answers everywhere. A second proof adds
  maintenance, not coverage.
- Pattern. CI reaching into `examples/` for a fixture makes a consumer a contract
  source — exactly what §Consumers forbids. A test owns its fixtures; no test may
  `#[path]`-include consumer sources, and none other does (verified 2026-08-30:
  this is the tree's only such include).

- REMOVED: sdk/streamlib-sdk/src/hello_streamlib_example_e2e.rs
- REMOVED: hello_streamlib_example_e2e
  The module declaration at `sdk/streamlib-sdk/src/lib.rs:296` and the comment at
  `sdk/streamlib-sdk/Cargo.toml:38` go with the file. The plan's verify comment
  naming the walker (`ARCHITECTURE.md:45`) is deleted in the same PR — a factual
  record riding the change; §Product's DECIDED text itself does not change, and
  no replacement test is owed.

## MODIFIED: engine-tree narration of the pre-pivot consumer doctrine

Prose that states the old doctrine as fact, rewritten to §Consumers in the sweep's
own PR — these are the surfaces a reader actually hits when asking what `packages/`
is:

- `Cargo.toml:27-42` — the workspace-members comment still narrates consumers as
  "headed to `tatolab/streamlib-packages` (#1672), deny-ruled against editing",
  breakage as "upgrade backlog", and names `packages/escalate` and `packages/core`
  as live engine-side schema holders. Rewritten: `test-fixtures` is the one member;
  everything else under both trees is a consumer with disposition per §Consumers.
- `xtask/src/check_clock_usage.rs:38-48` — same narration in the scan-roots doc
  comment, same two dead package names.
- `.claude/scripts/ship-change-removed-gate.sh` `engine_side_packages` keeps
  `escalate` and `core` entries for directories that no longer exist — shrunk to
  `test-fixtures`. Behavior-neutral (the exclusion loop derives from the live
  tree), a narration fix, not a gate change.

## ADDED: the conversion backlog, filed

§Consumers makes every pre-pivot consumer neither retired nor held conversion
backlog; this change projects that category into the tracker so it stops being
plan-text-only. One tracked issue per consumer, each written to the ticket-intent
convention (what we want / what exists today / why it moves), filed in this change,
executed later and separately:

- `examples/audio-mixer-demo`, `examples/microphone-reverb-speaker` — rewrite
  against the shipped audio surface (device seam, window contract, delivery
  profiles).
- `examples/camera-plugin-sdk-compute` — its plugin-SDK form is deleted machinery;
  rebuilt as a compute-kernel example against kernels-as-objects.
- `examples/cuda-fisheye-detection` — rewrite against the current interop surface.
- `examples/raytracing-showcase` — rewrite against the shipped ray-tracing kernel
  surface (Python or Rust form settled at conversion, per the authoring doctrine).
- `examples/tokio-integration` — Rust authoring stays supported; rewrite as a plain
  cargo project against the `streamlib` crate.
- The halftone effect — rebuilt as a Python-authored kernel example from the mined
  source (see the camera-deno-subprocess bullet above).

The already-converted pair (`camera-display`, `camera-python-effects`) needs no
issue; lag-by-design has already ended for them. No first-party Python integration
package is commissioned here — `packages/` after the sweep is `test-fixtures` plus
the held consumers, and the reborn-package doctrine activates when the first real
integration wants a home; the publish path (the wheel's PEP 503 index) requires no
work until then.

## Not in this change

- No conversion is executed — each backlog issue is its own later work,
  from-scratch per doctrine.
- Held consumers are not touched: codec blocks
  (`packages/h264`, `packages/h265`, `packages/jpeg`, `packages/opus`,
  `packages/mp4`, and the five held examples), networking (`packages/moq`,
  `packages/webrtc`, three examples), audio plugins (`packages/clap`), screen
  capture (`packages/screen-capture`, `examples/screen-recorder`). Each deletes
  through its domain's future align, per the hold-until-mined bullet.
- No examples-repository split, no CI presence for the showcase, no new
  distribution mechanism — all decided against or deferred in §Consumers.
