# Manifest extraction as a shared source-scan capability

## Trigger

Reach for this when touching how a package's `processors:` manifest section
relates to its `#[processor(...)]` attributes — deriving the manifest from
code, deciding where the attribute grammar lives, or reconciling the version-
free attribute grammar with the release-core versions the catalog carries.

## Decision

The `#[processor(...)]` attribute is the single source of truth for a
processor's identity, execution mode, and ports. The `processors:` manifest
section is therefore *derived* from the attribute usage in code, by a
source-scan that reads a crate's `src/` **without compiling it into the host**
and produces the manifest-shaped processor list.

The attribute is parsed by exactly one parser,
`streamlib_processor_extract::grammar`. Both readers of code-as-truth call it:
the proc-macro (`streamlib-macros`) at expansion, and the source-scan extractor
over the tokens a `syn`-parsed attribute carries. The grammar and the extractor
live together in the small non-proc-macro crate `streamlib-processor-extract`,
which both `streamlib-macros` and the build seam (`streamlib-pack`) depend on.

The scan produces a `ProcessorSchema` whose `name` is the identity's `Type`
segment, whose `version` is the `0.0.0` version-free sentinel, and whose ports
carry resolve-free `PortSchemaSpec::Specific(@org/package/Type@0.0.0)` idents.
Version resolution is the consumer's projection: the publish-time catalog
projects each version-free ref to a release-core `SchemaIdent` — the owner
package's version for a locally-owned schema, the owning dependency's version
for an external one — the same way it already projects a bare `Named` ref
through the `schemas:` map. A `-dev.N` package projects to its release-core
version at build time; that is a build-time projection, not a publish gate.

## Rejected alternatives

- **Grammar in the proc-macro crate.** A `proc-macro = true` crate can only
  export procedural macros, never a library function another crate links — so
  the source-scan could not reuse it, and would need a second parser that
  drifts against the first.
- **A second parser in the extractor.** A parallel grammar is the parallel
  abstraction the engine doctrine forbids; the two would diverge silently.
- **Extraction inside the engine runtime crate.** A `syn`-AST scan over an
  uncompiled crate needs none of the engine runtime (RHI, IPC, executor) and
  must not pull it into the build seam.
- **Extraction only inside `streamlib-pack`.** Pack is the natural consumer,
  but the grammar must also be shared with the proc-macro, and a future
  live-submit path needs the extractor without the whole pack crate.

## Consequences

- One grammar serves both the macro and the scan; the macro expands
  identically after the move (its unit + integration tests are unchanged).
- The scan is a lean text-in / manifest-out transform reusable by any build or
  submit path.
- `extract_rust_processors` is the RAW scan: it visits every `.rs` under `src/`,
  including platform arms a given host does not compile (`linux/` vs `apple/`)
  and parked directories (`_apple_impl_pending_/`), so two platform arms that
  both declare the same processor both surface. `extract_reachable_rust_processors`
  resolves that raw scan to the set the build **target** actually compiles: it
  walks the module tree from the crate root (`lib.rs` / `main.rs`), follows each
  `mod` the way `rustc` resolves module files (honoring `#[path]`), and evaluates
  the `#[cfg(...)]` predicate on every `mod` and every `#[processor(...)]`-bearing
  struct against a `ModuleReachabilityTarget` (the target's cfg atoms:
  `target_os` / `target_arch` / `target_family` / features / family flags). The
  parked-directory convention is not special-cased: a parked module is declared
  `#[cfg(any())]`, an always-false predicate, so it is skipped by the same cfg
  rule `rustc` applies — one rule, not a hard-coded directory name. This
  reachability resolution is the precursor that makes extraction sound enough to
  replace the hand-authored `processors:` as the authoritative truth-source, and
  a drift check between the two a hard `pkg build` error without false positives
  on cfg-gated packages.
