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
source-scan that reads a package's `processors/` directory **without compiling
it into the host** and produces the manifest-shaped processor list.

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

## Polyglot analogue: extraction is import

The same "code is the truth-source" decision holds for the Python and Deno
SDKs, but the mechanism inverts to fit each runtime. A `syn` AST scan reads
Rust source without running it; Python and Deno have no comparable
parse-without-run path for decorator semantics, so **extraction is import**:
applying `@processor(...)` at import time registers the processor's structured
identity into a process-global registry (`_processor_registry`), and the
per-language extractor imports every processor module and enumerates what
registered. Only the package identity (`package: { org, name, version }`) is
still read from `streamlib.yaml`; the decorator no longer validates its short
name against a hand-authored `processors:` list — the decorator *is* that list.

> ~~The per-language extractor imports every top-level module beside the
> `streamlib.yaml`.~~ — Superseded 2026-07-27 by
> `sdk/streamlib-python/python/streamlib/extract_processors.py` and
> `sdk/streamlib-deno/extract_processors.ts`: discovery is a recursive walk of
> `<package_dir>/processors/`. A module authored beside the `streamlib.yaml` is
> not a processor module, and a package with no `processors/` directory
> extracts to the empty set. Superseded again 2026-07-27 in the other direction
> by `streamlib_processor_extract::extract_rust_processors`, which walks the
> same `<package_dir>/processors/` root and refuses a package without it, plus
> `streamlib_processor_extract::crate_root`, which projects that root into a
> generated crate root: `processors/` is now the one discovery root for all
> three languages rather than the polyglot analogue of a Rust-only `src/`.

The extractors run in a fresh subprocess (`python -m
streamlib.extract_processors <dir>` / `deno run --allow-read
extract_processors.ts <dir>`) so the registry starts empty; the in-process
entrypoints clear the registry themselves. Output is sorted by joined
schema-ident string for determinism regardless of import order.

The import-runs-code property is the cost of the inversion: a processor module
whose third-party imports are unavailable cannot be enumerated, whereas the
Rust AST scan is inert. Extraction therefore assumes the package's dependencies
are installed — true at `pkg publish` time, unlike the Rust AST walk.

## The publish-time drift gate

> ~~The gate is named the "pkg-build" gate and fires from `streamlib pkg build`
> as well as `pkg publish`.~~ — Superseded 2026-07-27 by the removal of the
> `pkg build` verb (`tools/streamlib-cli/src/main.rs`). Distribution is
> by-version through `pkg publish`; the gate itself is unchanged, it now has
> exactly one CLI entry point.

`streamlib pkg publish` derives the processor set from code —
Rust in-process, Python/Deno via the subprocess extractors — and refuses to
build a distributable `.slpkg` whose committed `processors:` section disagrees.
The comparison is a **language-uniform identity surface**: processor `Type`
name, execution mode, and each port's name + schema-type (or `any`). It
deliberately excludes fields no code scan produces uniformly or that are
authored/build-derived rather than code-derived — `version` (the release-core
projection is a build concern), `entrypoint` (author/loader concern), the
`config` binding, `description`, and the consumer-side `delivery_profile`
(which the Python/Deno wire shape does not carry uniformly). What remains is
exactly the surface a stale hand-authored
`processors:` would misstate: a processor added, removed, or renamed in code; a
port added, removed, reordered, or re-typed. The committed manifest stays the
carrier of the excluded authored/build fields — the `processors:` section is not
auto-overwritten, it is validated against code.

Rust drift is always enforced (the `syn` scan is always runnable in-process). A
Python/Deno extractor that cannot **run** — the runtime is absent, its import
failed, or (Deno) it is unconfigured — is a logged skip, not a build break:
extraction-is-import needs the runtime present, and a `pkg publish` that merely
bundles a Python/Deno package as source on a host without that runtime must
still work. A skipped language's committed processors are excluded from the
comparison rather than falsely flagged. A malformed *output* from an extractor
that DID run is a hard error (the extractor ran and produced garbage — a real
bug, not an absent runtime). Real Python/Deno drift enforcement is exercised
live, where the runtime is present.

The gate is scoped to the distributable `.slpkg` authoring path; the runtime
orchestrator's staged-directory materialization assembles an already-validated
artifact and does not re-run it.

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
- **`cfg-expr` / `target-lexicon` for the overlap search.** They ship the target
  coherence rules as data rather than the hand-written model below. Neither is
  in the tree today, so adopting one is a new-dependency decision for a
  build-seam crate that currently pulls only `syn` / `quote` / `toml` — and the
  model needs one more thing than either provides: an explicit "this fact is
  unknown, leave the pair unproven" answer, which is what keeps the refusal
  sound. Worth revisiting if the coherence rules grow past the target-atom
  cluster the model relates today.

## Consequences

- One grammar serves both the macro and the scan; the macro expands
  identically after the move (its unit + integration tests are unchanged).
- The scan is a lean text-in / manifest-out transform reusable by any build or
  submit path.
- `extract_rust_processors` is the RAW scan: it visits every `.rs` under
  `processors/`, including platform arms a given host does not compile
  (`camera_linux.rs` vs `camera_apple.rs`) and parked directories
  (`_apple_impl_pending_/`), so two platform arms that both declare the same
  processor both surface.
  `extract_reachable_rust_processors`
  resolves that raw scan to the set the build **target** actually compiles: it
  enumerates the top-level arms under `processors/` the way the generated crate
  root declares them (a directory backed by `mod.rs` keeps directory ownership,
  a flat `.rs` is a flat arm), follows each
  `mod` the way `rustc` resolves module files (honoring `#[path]`), and evaluates
  the `#[cfg(...)]` predicate on every `mod` and every `#[processor(...)]`-bearing
  struct against a `ModuleReachabilityTarget` (the target's cfg atoms:
  `target_os` / `target_arch` / `target_family` / features / family flags). The
  parked-directory convention is not special-cased: a parked module declares
  `#![cfg(any())]`, an always-false predicate, so it is skipped by the same cfg
  rule `rustc` applies — one rule, not a hard-coded directory name. This
  reachability resolution is the precursor that makes extraction sound enough to
  replace the hand-authored `processors:` as the authoritative truth-source, and
  a drift check between the two a hard `pkg publish` error without false positives
  on cfg-gated packages.

## Grouping by processor id: what is refused, what is data

Once two files under one `processors/` tree can declare the same processor id
under different `#[cfg]`, three things become possible. Two are refused at the
scan; the third is the datum the scan now produces.

**Overlap is refused, with a witness.** Two arms some build target compiles
both of derive one `processors:` entry between them: the section keeps only the
`Type` segment, so the entry that ships is whichever arm the walk reached
first, and the drift check — keyed by that same name — collapses the pair
before it compares. The failure is silent at both seams. It is refused at the
scan instead, proven two independent ways. The target-resolved walk needs no
reasoning: it resolved one concrete target and collected the name twice. The
across-every-target walk brute-forces satisfiability of the two arms' conjoined
predicates over only the atoms those predicates themselves mention, and refuses
**only on a satisfying assignment it can print**. That search carries a
deliberate domain model of how `rustc` sets these atoms: `target_*` keys are
modelled single-valued except `feature` / `target_feature` /
`target_has_atomic`; the `unix` / `windows` families are mutually exclusive,
spelled interchangeably as a bare flag or a `target_family` value, with
`windows` holding exactly one `target_os`; and a known `target_os` fixes the
families its target defines, both ways. Without the first rule, `target_os =
"linux"` against `any(target_os = "macos", target_os = "ios")` reads
satisfiable and every platform-split package fails its own build. Without the
last, `target_os = "ios"` against `not(unix)` reads satisfiable and a platform
split with a non-unix fallback arm fails the same way.

**The model relates one cluster of target atoms, and prunes rather than
guesses outside it.** The cluster is `target_os`, `target_family` and the bare `unix` /
`windows` flags. `rustc` fixes every other target key against those and against
one another too — no target is both `target_env = "msvc"` and `target_os =
"linux"`, none is both `target_arch = "wasm32"` and `target_env = "msvc"`, no
`target_vendor = "apple"` target is `not(unix)` — and the model holds none of
those facts, so an assignment that pins a key from outside the cluster while a
second target atom is decided is dropped instead of printed as a proof. Inside
the cluster the same discipline applies to what the rules cannot decide: a
`target_os` outside the OS → family table decides nothing about families, and
a `target_family` outside `unix` / `windows` defines neither flag, which
deliberately drops `wasi` (genuinely both `wasm` and `unix`, the multi-valued
case single-valued modelling gives up). Every rule and every gap therefore
prunes, so the error direction is toward missing an overlap rather than
inventing one — on the single assumption the model cannot check, that a value
a predicate names is a value some target defines. The cost of a missed
detection is bounded by the concrete-target net: the host that actually
compiles both arms still fails.

**Divergence is refused over the whole derived projection, not just ports.** The
`processors:` section is derived from whichever arm the publishing host
compiles, so a difference in ANY field the manifest entry carries — the full
`@org/package/Type` identity, execution, ports, scheduling, description, the
config binding — makes the shipped manifest host-dependent. That is a wider
surface than the language-uniform drift surface above, deliberately: drift
compares a *hand-authored* manifest against *code*, where the excluded fields
are authored rather than derived, while divergence compares two pieces of code
that must derive the same entry. Port and execution differences are named by
the same comparator the drift report uses, generalized from "manifest vs code"
to a labelled two-sided comparison — one diagnostic with two callers, not two
copies. Grouping is by `Type` name rather than the full identity because that
is the only segment the derived entry keeps: the attribute's `@org/package` is
dropped, and at load the runtime composes each processor's structured ident
from the package's own org / name plus the short name. Two arms sharing a
`Type` fold into one manifest entry and one composed ident whatever
`@org/package` their attributes named, so they surface here as a divergence
rather than passing as two unrelated processors.

**A gap is not an error — it is availability.** A package that declares a
processor on some targets and on none of the others is ordinary. Which targets
those are is carried as a per-processor `#[cfg(...)]` **predicate** — the
disjunction over each declaring arm's conjoined predicates, `None` meaning
unconditional — never as an enumerated target set. There is no closed target
universe (an arm may gate on `redox`, `android`, a cargo feature, a custom cfg),
so a fixed list would go stale and misreport; the enumerated answer is derived
on demand for whatever targets a caller cares about, through the one cfg
evaluator rather than a second implementation of cfg semantics. The crate-root
generator's `export_plugin!` outer gate is that same disjunction: "the target
compiles at least one of this package's processors".

Availability is scan output, consumed in-process. It is deliberately NOT added
to `streamlib.yaml`'s `processors:` section: that section's comparison surface
is language-uniform by design, and Python and Deno have no cfg to project.

A `#[cfg(feature = "…")]`-gated processor is the one case the target-resolved
walk cannot decide correctly on its own — the scan target derives os / arch /
family from the running host and cannot know which features a downstream build
enables, so the processor evaluates false and leaves the derived set. It bites
one seam later as a confusing drift error ("listed in `processors:` but no
longer declared in code"), so the walk warns at the prune site with the file,
the predicate and the undefined feature rather than leaving the absence
unexplained. The warning is raised only where a `#[processor(...)]` really left
the set — the prune site re-walks what it pruned with cfg resolution off and
stays silent for a feature-gated helper module, whose absence explains nothing.

Both refusals reach a package through crate-root generation, so they gate every
build and every `pkg publish` of a package that DECLARES a generated crate root.
A package that commits its own crate root (the one in-tree host rlib) is scanned
across targets only by the extractor's own tests: cross-target divergence there
would surface on a host that compiles both arms rather than at generation. That
exposure follows from generation being opt-in, and is accepted rather than
worked around with a second discovery path.
