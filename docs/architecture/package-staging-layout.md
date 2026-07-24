# Package staging layout — the authored-tree mirror

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Verify against current code before relying on anything load-bearing.

## The invariant

**A staged or published package is a faithful mirror of the authored
source tree.** No language relocates the developer's files. Whatever
layout the author wrote — entrypoints, helper modules, per-language
config, assets — is exactly the layout that lands in the staged package
cache slot and in the published `.slpkg`. Build *outputs* are the only
additions, and they go in well-known additive locations that never
collide with authored source.

This is engine-model territory ([CLAUDE.md "The StreamLib Engine
Model"](../../CLAUDE.md#the-streamlib-engine-model)): there is one way a
package is laid out, and all three runtimes use it. The payoff is that
every relative path the author wrote resolves identically in dev and in
the artifact:

- > ~~A processor's sibling `streamlib.yaml` (the `@processor` decorator
  > and the Rust/Python proc-macros resolve the manifest as a sibling of
  > the source file).~~ — Superseded 2026-07-19: the decorator / proc-macro
  > is the truth-source and reads **nothing** from disk at decoration time
  > (see [`zero-ceremony-authoring.md`](zero-ceremony-authoring.md) and the
  > `@processor` docstrings). Identity, execution, and ports are declared
  > in code; a bare module with no `streamlib.yaml` is a working local
  > processor. The mirror invariant still matters for *authored assets a
  > package ships and reads at runtime* (below), not for manifest lookup.
- A Deno processor's `./_generated_/<org>__<pkg>/<type>.ts` import (only
  when the consumer opts into `streamlib generate` typed views).
- Any asset a package ships and reads at runtime (`./shaders/x.wgsl`,
  `./models/y.bin`, embedded video/html/data) — at its authored path.

The moment a package's files are relocated during staging, one of those
relative resolutions breaks for a layout the author never sees and can't
predict. Mirroring is what keeps the authored mental model intact.

## What "mirror" means per language

`streamlib_pack::assemble_artifact` is the single routine that turns a
package source dir into a staged dir or a `.slpkg`. It bundles the full
authored tree via `collect_source_tree` for every language that ships
runnable/buildable source — Python, Rust, **and** Deno — archiving each
file at its authored relative path:

| Language | Authored source that travels (mirror) | Additive build output (not authored) |
|---|---|---|
| **Python** | full tree: every `.py` + `pyproject.toml` + data / assets / models | a provisioned `.venv/` (orchestrator), SDK wire vocab in `streamlib/_generated_` inside that venv |
| **Deno** | full tree: every `.ts` + `deno.json` + `.npmrc` + assets | package-local `_generated_/` wire vocab (orchestrator) |
| **Rust** | full crate source: `Cargo.toml` + `src/` + data / assets | prebuilt cdylib at `lib/<triple>/` (one per packing host) |

The entrypoint declared in `streamlib.yaml` (`module.py:default`,
`module.ts:default`) names a file **at the package root**, beside
`streamlib.yaml` — never under a language subdir. The pack assembler
validates the entrypoint exists, but the file itself travels through
`collect_source_tree` like any other source file; the emitter dedups the
overlap.

### Build outputs are additive, never relocations

The distinction that makes the mirror work: authored source is copied
verbatim; build outputs are *added* in reserved locations.

- `lib/<triple>/<crate>.so` — the Rust cdylib. `collect_source_tree`
  excludes `lib/`, so the prebuilt and the source tree never collide
  ("sdist + one-triple wheel").
- `_generated_/` — JTD codegen output. **Excluded from the shipped
  source** as a per-consumer artifact (regenerated at stage time from
  the package's schemas), and regenerated into the staged dir by the
  orchestrator (below). Deno packages carry a package-local
  `_generated_/` next to their `.ts`; Python's SDK wire vocab is
  generated into `streamlib/_generated_` inside the venv.
- `.venv/` — the Python dependency environment, provisioned into the
  staged dir.

### What is never source

`streamlib_pack::is_non_source_artifact` is the single definition of
"not authored source," shared by `collect_source_tree` and the
orchestrator's input fingerprint. It excludes build outputs, VCS, and
dev caches: `target`, `lib`, `_generated_`, `.venv` / `venv`,
`node_modules`, `__pycache__`, `.git`, `Cargo.lock`, the lint/test
caches, `.streamlib-build.json`, plus `*.slpkg` / `*.egg-info` /
`*.pyc`. These directory names are **reserved** — a package must not use
them as its own authored source dirs.

## Regenerating build outputs at stage time

`streamlib_build_orchestrator` materializes a source package into the
cache slot: `assemble_artifact` (mirror the source) → per-language
provision tails (regenerate build outputs into the same staged temp dir)
→ atomic rename. Because provisioning writes into the build-to-temp
directory, the single atomic rename carries the outputs into place; no
second rename.

- `provision_python_venv` — `uv venv` + install deps + generate the
  SDK's `streamlib/_generated_` wire vocab into the venv + pre-warm
  `.pyc`.
- `provision_deno_typescript` — run JTD codegen
  (`RuntimeTarget::Typescript`) into the staged package's `_generated_/`,
  resolving schema deps (e.g. `@tatolab/core`) from the package source via the
  env-aware resolver. It **always** materializes `_generated_/` for a
  Deno package (even with no external schema deps) so the directory's
  presence is a reliable "codegen tail ran" marker.

Both tails are no-ops for a package without that runtime.

### Cache-reuse integrity

`IfStale` reuse of a fingerprint-matched slot is gated on each
language's build output being present:
`cache_slot_is_reusable(has_python, venv_exists, has_deno,
generated_exists)` requires a Python slot's `.venv` and a Deno slot's
`_generated_`. A slot missing either (out-of-band deletion, or a slot
staged by an older orchestrator that never ran that provision tail)
re-stages instead of being reused broken. The per-package input
fingerprint (over authored source, per `is_non_source_artifact`) is the
staleness oracle for normal source edits; the build-output guards cover
the integrity gap the fingerprint alone can't see.

## Anti-pattern: relocating authored files

Do not move an authored file to a different path during staging. The
failure mode this rule prevents: a language whose assembler dropped the
entrypoint into a subdir (`deno/<file>.ts`) while leaving
`streamlib.yaml` at the root, and which bundled only the entrypoint
rather than the full tree. The result was a staged layout the author
never wrote — `_generated_` and `.npmrc` and assets never travelled, and
the symptom pointed at the decorator rather than at the relocation that
actually caused it. The fix was not to teach the decorator to climb
directories; it was to stop relocating, so the authored layout and the
staged layout agree by construction.

> ~~The historical trigger for this rule was `@processor` reporting
> `streamlib.yaml not found` after a relocation.~~ — Corrected 2026-07-19:
> the decorator no longer reads a sibling `streamlib.yaml` at all
> ([`zero-ceremony-authoring.md`](zero-ceremony-authoring.md)), so that
> particular symptom can no longer occur. The rule still holds for the
> asset / `_generated_` / `.npmrc` paths that *are* resolved relative to
> the authored layout.

If a new runtime or a new asset class tempts a relocation "for
symmetry," stop — the symmetry that matters is dev-layout ==
staged-layout, and that is achieved by mirroring, not by moving.

## Reference

- **Assembler**: `assemble_artifact`, `collect_source_tree`,
  `is_non_source_artifact` in `tools/streamlib-pack/src/lib.rs`.
- **Orchestrator**: `materialize_package_dir`, `cache_slot_is_reusable`
  in `tools/streamlib-build-orchestrator/src/lib.rs`;
  `provision_python_venv` (+ `ensure_streamlib_generated_in_venv`) in
  `src/python_venv.rs`; `provision_deno_typescript` (+
  `staged_package_has_deno`) in `src/deno_codegen.rs`.
- **Consumers of the layout**: the entrypoint resolution in
  `sdk/streamlib-deno/subprocess_runner.ts` (`resolveDenoModulePath`
  resolves against the package root); the Deno spawn op
  (`--config <pkg>/deno.json`) in
  `runtime/streamlib-engine/src/core/compiler/compiler_ops/spawn_deno_subprocess_op.rs`.
- **Related**:
  - [`package-source.md`](package-source.md) —
    how a package resolves and publishes by version (the *what travels
    where* of distribution; this doc is the *how it's laid out* of
    staging).
  - [`schema-identity-and-packaging.md`](schema-identity-and-packaging.md)
    — schema identity + the `schemas:` map the codegen tree-shakes
    against.
  - [`compute-kernel.md`](compute-kernel.md) — companion "single
    canonical abstraction" engine-model pattern.
