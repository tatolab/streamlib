# Dependency reconciliation at package build

## Trigger

Reach for this when touching how a package's declared `dependencies:` relates to
what its code references — deriving the dependency set, deciding what an
undeclared or unreferenced dependency does at `pkg build`, or wondering why the
reconciler reads the manifest rather than re-scanning code.

## Decision

`pkg build` reconciles the hand-declared `dependencies:` against the dependency
set *derived* from the package's references, for the distributable `.slpkg`
target only (`streamlib_pack::dependency_reconcile`):

- A **referenced-but-undeclared** package is a hard error, carrying a
  `streamlib add @org/name@<version>` fix-it. The manifest must declare every
  package its code references.
- A **declared-but-unreferenced** package that is not marked `runtime: true` is
  a non-fatal prune warning — dead-weight in the dependency list.
- `runtime: true` on a dependency keeps a runtime-composition dependency that
  imports none of the referenced package's schema types.

The referenced set is derived from the **manifest** — every `schemas:
External { package }` import plus any resolved `Specific(SchemaIdent)` port id —
not from a fresh per-language code scan. The committed `processors:` block is
already pinned to code by the processor-manifest drift gate, so deriving from
the manifest *is* deriving from code, and the manifest's `schemas:` map is the
only place a bare `schema: VideoFrame` reference's owning `@org/package` is
recorded.

## Rejected alternatives

- **Scan code for the referenced set (the processor-extract crate).** The
  cross-language processor surface collapses each port schema to its bare `Type`
  short-name and drops `@org/package`, so it cannot name the owning dependency;
  a Rust-only raw scan could, but the owning package for a bare port name lives
  in the manifest's `schemas:` map regardless. Deriving from the manifest is
  both language-uniform and the only complete source.
- **Prune destructively — rewrite the shipped manifest to drop the dead
  dependency.** A published `.slpkg` manifest must stay byte-identical to the
  one an orchestrator `StagedDir` build produces from the same source; pruning
  only on the `Slpkg` path would diverge the two. Pruning is therefore a warning
  that tells the author to remove the dependency (or mark it `runtime: true`),
  never a silent under-the-author manifest rewrite.
- **Make an unreferenced dependency a hard error too.** Over-rotates: a
  legitimate runtime-composition dependency imports no schema types, and the
  `runtime: true` marker already expresses that intent. Undeclared references
  are always a manifest lie and stay hard; unreferenced ones are advisory.

## Consequences

- The `schemas:` map is load-bearing for dependency identity, not just schema
  resolution: an external schema import is the machine-checkable evidence that a
  declared dependency is needed.
- A package that composes another package's processors at runtime without
  importing its schemas must mark that dependency `runtime: true`, or accept the
  prune warning.
- The reconcile runs after the path-artifact gate, so a dev-only package
  carrying a `patch:` path override never reaches it — its dependency list is
  reconciled only once it is built as a standalone, path-free artifact.
