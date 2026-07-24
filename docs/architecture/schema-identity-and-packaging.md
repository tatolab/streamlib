# Schema identity & packaging

> Current known state of the schema-identity surface. Subject to
> staleness or drift — verify against the code before relying on any
> claim. Not authoritative, not enforcement.

## What this document describes

The architecture surface shared across the `streamlib-idents` and
`streamlib-jtd-codegen` crates: identifier grammar, package manifest
formats, dependency resolver, lockfile, the codegen pipeline, and the
anti-patterns the design rules out. Every claim below describes
behavior that ships in current code.

## Why this exists

Through 2025–early 2026 the schema-identity surface drifted across
three independent strands:

- **Reverse-DNS schema IDs** (`com.tatolab.videoframe@1.0.0`) embedded
  in YAML metadata blocks, parsed by ad-hoc `from_str` impls in
  Rust + Python + TypeScript. Each runtime had its own parser; minor
  variations in tolerated whitespace / case / trailing data accumulated
  silently.
- **Per-language manifest extensions** (`[package.metadata.streamlib]`
  in `Cargo.toml`, `[tool.streamlib]` in `pyproject.toml`, an
  ungoverned `streamlib` block in `deno.json`). Three sources of truth
  describing the same set of facts.
- **Incomplete distribution attempts** (`embedded_schemas.rs`'s
  hand-curated match statement, ad-hoc `.slpkg` archive experiments,
  schemas that lived only in `runtime/streamlib-engine/schemas/` with no
  publication story).

The fix is one cohesive architecture covering identifier grammar,
package manifest, dependency resolution, code generation, and
distribution.

## Architectural decisions

These are the load-bearing design choices the current code rests on.
Relaxing any of them brings back the failure mode this architecture
was shaped against.

### Decision 1 — `@org/package/Type@version` identifier grammar

Schema identifiers take the npm-style form `@tatolab/core/VideoFrame@1.0.0`:
scoped org, explicit package, PascalCase type name, semver. The
grammar (BNF):

```ebnf
identifier   ::= "@" org "/" package "/" type "@" version
org          ::= [a-z] [a-z0-9-]*
package      ::= [a-z] [a-z0-9-]*
type         ::= [A-Z] [A-Za-z0-9]*
version      ::= major "." minor "." patch
major        ::= [0-9]+
minor        ::= [0-9]+
patch        ::= [0-9]+
```

Worked examples:

| Identifier | org | package | type | version |
|---|---|---|---|---|
| `@tatolab/core/VideoFrame@1.0.0` | `tatolab` | `core` | `VideoFrame` | `1.0.0` |
| `@tatolab/h264/EncodedVideoFrame@1.0.0` | `tatolab` | `h264` | `EncodedVideoFrame` | `1.0.0` |
| `@tatolab/camera/CameraConfig@1.0.0` | `tatolab` | `camera` | `CameraConfig` | `1.0.0` |
| `@streamlib/escalate/EscalateRequest@1.0.0` | `streamlib` | `escalate` | `EscalateRequest` | `1.0.0` |

Pre-release / build metadata (the `1.0.0-rc.1+sha.deadbeef` shape) is
deliberately not supported in v1. Re-introduce when a real consumer
needs them — adding now creates parser surface that has no caller.

### Decision 2 — structured-everywhere wire format

**Every reference to a schema identifier is a structured record on
every wire surface.** No joined string is ever the source of truth.

```yaml
# Wire shape (typed YAML / JSON) — four fields, never a single string:
org: tatolab
package: core
type: VideoFrame
version: 1.0.0
```

Surfaces this rule covers:

- IPC envelopes (`escalate_request` / `escalate_response`, surface-
  share, iceoryx2 payloads).
- Codegen-emitted const records (`SCHEMA_IDENT: SchemaIdent { … }`).
- Graph JSON (the runtime's serialized pipeline graph).
- Embedded-schema lookup keys (replaces `embedded_schemas.rs`'s
  `match` on string).
- Lockfile entries (`streamlib-codegen.lock`).

The `Display` impl on `SchemaIdent` produces the joined `@org/pkg/Type@v`
form for human-facing surfaces (logs, error messages, CLI output).
**The joined form is render-only — it never round-trips back through
a parser at the structured boundary.**

#### Why structured everywhere

Three independent reasons converged on this answer:

- **AI determinism.** Future agents (and current ones) read code to
  derive contracts. A `parse("@org/pkg/Type@v")` API is one more
  place where an LLM has to guess about whitespace / Unicode /
  trailing-data tolerance. A struct literal with four named fields
  is unambiguous-by-construction.
- **Web-UI / API-server readability.** External consumers reading
  the runtime's API responses get four typed fields; they don't
  have to pattern-match strings to figure out which package owns
  which type.
- **Type-system over convention.** A `Org` newtype with a private
  constructor + a validating `new()` makes "invalid org" *unrepresentable
  in the type system after the validation gate*. Convention-driven
  parsing routes around this.

#### Carve-out: `SemVer` parses from `"1.2.3"`

The structured-everywhere rule applies to *identifiers*, not to
every primitive. `SemVer` has a single canonical string form
(`1.2.3`) that's universally agreed across cargo / npm / pip /
deno; representing it as `{major: 1, minor: 2, patch: 3}` in YAML
would be surprising. `SemVer` is therefore deserialized from a
string via the typed-deserialization pathway. This is not a
weasel-out — `SchemaIdent` is multi-field-glued-by-punctuation;
`SemVer` is single-canonical-string — the design line falls
between the two.

### Decision 3 — package-as-publication-unit

`streamlib.yaml` is the only place a `version` field lives. Schema
files declare `type` and content fields; **they do not declare a
version anywhere**. Bumping any type in a package bumps the whole
package's version (npm-style; Cargo-style; cargo-workspace-style).

Why: per-schema versions create N-dimensional matrices of "which
versions of which schemas are mutually compatible" that no consumer
ever actually reasons about. Publication-unit-scoped versions
collapse this to a single dimension per package.

A CI lint (`cargo xtask check-schema-versions`, wired into
`.github/workflows/check-schema-versions.yml`) rejects any schema
YAML declaring a top-level `version` key.

### Decision 4 — `streamlib-codegen.lock` for content-hash resolution

Every project that consumes packages (applications, examples) writes
a `streamlib-codegen.lock` next to its `streamlib.yaml`. The lockfile is
content-hash-pinned and diff-stable (sorted `BTreeMap` keys), so a
fresh checkout reconstructs the same generated bindings byte-for-byte.

Discipline (mirrors `Cargo.lock`):

- **Commit `streamlib-codegen.lock`** in applications, examples, and any
  non-publishable consumer.
- **Don't commit `streamlib-codegen.lock`** in publishable libraries — they
  inherit their consumer's lock.

### Decision 5 — `@tatolab/core` is the canonical wire vocabulary

The four wire-stable types every other package depends on
(`VideoFrame`, `AudioFrame`, `EncodedVideoFrame`, `EncodedAudioFrame`)
live in a single `@tatolab/core` package at `packages/core/`. This is
streamlib's `google.protobuf` analogue. `@tatolab/core` ships at
`1.0.0` from day one; breaking changes require a deliberate v2 bump
and downstream migration.

### Decision 6 — universal `streamlib generate` CLI

Code generation goes through `streamlib generate` (a subcommand on
the `streamlib` CLI). Non-Rust developers regenerate bindings without
ever installing rustup. `cargo xtask generate-schemas` remains as a
thin contributor-only wrapper. The library crate behind both is
`streamlib-jtd-codegen`.

### Decision 7 — sentinel-substitution codegen + deterministic ordering

Backend codegen (`jtd-codegen`) historically produced subtly different
field orderings + name manglings across runs and across backends
(rust / python / typescript). The fix is two passes:

1. **Sentinel substitution.** Replace cross-package type references
   with deterministic placeholder sentinels *before* invoking the
   per-backend codegen, then substitute back after. The backend
   never sees real cross-package references and can't disagree
   about how to mangle them.
2. **Deterministic field ordering.** A normalization pass that
   stable-sorts properties by name across all backends.

Together these eliminate the per-backend mangling drift that bled
silent fix-PRs every quarter.

## Manifest formats

`streamlib.yaml` has two flavors. The top-level shape distinguishes
them; pick by what your crate is.

### Package flavor

A publishable package — has its own `package` block.

```yaml
package:
  org: tatolab
  name: core
  version: 1.0.0
  description: Canonical wire vocabulary

dependencies: {}
```

Package metadata fields:

| Field | Required | Notes |
|---|---|---|
| `org` | yes | Validated against the `org` grammar. |
| `name` | yes | Validated against the `package` grammar. The full identifier prefix is `@{org}/{name}`. |
| `version` | yes | SemVer. The only place a version field lives. |
| `description` | no | Free-form prose. |

### Project flavor

A consumer project (application, example) — depends on packages but
isn't itself publishable.

```yaml
dependencies:
  "@tatolab/core": "^1.0.0"
  "@tatolab/h264":
    path: ../h264
  "@tatolab/moq":
    git: https://github.com/tatolab/moq
    rev: abc123def456
```

Three dependency source flavors are supported:

- **By version** (string form `"^1.0.0"` or `{ version: "^1.0.0" }`).
  Resolved by `@org/name` + version range against the configured
  package source — any location serving versioned `.slpkg` files
  (`file://` tree, HTTP mount, later a mesh peer or offline cache),
  read like another filesystem (see `package-source.md`). There is no
  central registry.
- **Path** (`{ path: ../foo }`). Local-filesystem dependency, used
  inside the streamlib monorepo for pre-publish work.
- **Git** (`{ git: <url>, rev: <commit-sha> }`). Pinned-commit-only;
  branch / tag refs are deliberately not supported, mirroring the
  workspace dep-pinning rule from `CLAUDE.md` (Conventions →
  Dependencies). Resolver fails loudly on a missing `rev`.

### Semver-range matching

The supported range operators are an npm-flavored subset:

| Operator | Example | Matches |
|---|---|---|
| Exact | `=1.2.3` or bare `1.2.3` | exactly 1.2.3 |
| At least | `>=1.2.3` | 1.2.3 or any later |
| Caret (post-1.0) | `^1.2.3` | same major, version ≥ input |
| Caret (0.minor) | `^0.2.3` | same minor (`0.2.x`), version ≥ input |
| Caret (0.0.patch) | `^0.0.3` | exactly 0.0.3 |
| Tilde | `~1.2.3` | same major.minor (`1.2.x`), version ≥ input |

These are exactly what `streamlib.yaml` declarations need today —
adding more operators is straightforward when a real consumer
appears.

## Lockfile shape

`streamlib-codegen.lock` is the resolved-set companion to `streamlib.yaml`.
(The plain `streamlib.lock` name belongs to the per-app modules lockfile —
see `package-development-model.md`.)
Wire shape:

```yaml
version: 1
packages:
  "@tatolab/core":
    version: 1.0.0
    source:
      kind: by-version
      url: file:///path/to/package-source
    content_hash: "sha256:0123456789abcdef…"
  "@tatolab/h264":
    version: 0.4.2
    source:
      kind: path
      path: ../h264
    content_hash: "sha256:fedcba9876543210…"
  "@tatolab/moq":
    version: 0.2.0
    source:
      kind: git
      url: https://github.com/tatolab/moq
      rev: abc123def456
    content_hash: "sha256:1111222233334444…"
```

| Field | Notes |
|---|---|
| `version` | Lockfile schema version. Currently `1`. Future format-incompatible bumps are explicit. |
| `packages` | `BTreeMap` keyed by `"@org/name"` — sorted iteration is what makes the lockfile diff-stable. |
| `version` (entry) | The concrete resolved SemVer (not a range). |
| `source` | Discriminated union: `by-version` / `path` / `git` (`tag = kind`, snake-case). |
| `content_hash` | Namespace-prefixed (`sha256:…`) so future hash-algorithm migrations don't break parsing. |

The content hash is computed over the resolved package's contents
(deterministic pass over schemas + manifest). It's the load-bearing
primitive that lets a determinism gate prove committed `_generated_/`
matches the lockfile's inputs.

## Sentinel-substitution codegen contract

Code generation runs in three passes:

1. **Resolve** — read `streamlib.yaml` + `streamlib-codegen.lock`, walk the
   dependency graph, produce the full set of `(SchemaIdent, JtdSchema)`
   pairs to generate.
2. **Substitute → generate → substitute back.**
   - Pre-pass: replace every cross-package `SchemaIdent` reference in
     each schema's JTD definition with a deterministic placeholder
     sentinel (`__STREAMLIB_REF_<hash>__`).
   - Per-backend codegen: invoke `jtd-codegen --target {rust,python,typescript}`
     against the sentinel-substituted schemas. The backend never sees
     a real cross-package reference and can't mangle one inconsistently.
   - Post-pass: substitute the sentinels back to native cross-package
     `import`s in each backend's emitted code.
3. **Order** — stable-sort properties by name in all generated types
   (a normalization pass that makes the output diff-stable across
   backends and across runs).

Then the generated files are written to each consumer's `_generated_/`
directory.

The `streamlib-jtd-codegen` crate owns this pipeline. The
`streamlib-idents` crate owns the structured types the pipeline reads
and writes (`SchemaIdent`, `SemVer`, `Manifest`, `PackageMetadata`,
`Lockfile`) and the resolver that walks `streamlib.yaml` (`resolve`,
`ResolvedPackages`). `Manifest` deserialization tolerates runtime-side
fields (`processors:`, `env:`) without `deny_unknown_fields` rejection
so one `streamlib.yaml` carries both the schema-identity surface and
the runtime configuration.

### Root-name sentinel — sibling pattern at the `--root-name` boundary

Cross-package references aren't the only place `jtd-codegen` v0.4.1
mangles names. The same backend bugs (digit-boundary lowercasing
across all three; Python-only acronym upcasing; inconsistent
`--root-name` honoring across emit shapes) apply to **the root type
name `--root-name` declares**. `H264DecoderConfig` lands as
`H264decoderConfig` in TypeScript output even though that exact
spelling was passed on the CLI. The fix mirrors the cross-package
sentinel pattern at a different layer: pass a sentinel as
`--root-name` that no backend mangles, then literal-substitute the
sentinel back to the schema's `metadata.name`-derived identifier in
each post-processor.

Implementation: `ROOT_NAME_SENTINEL` const in
`streamlib-jtd-codegen::lib.rs` carries `"StreamlibCanonRoot"` as
the chosen sentinel value. Per-language `post_process_*` functions
end with a single `code.replace(ROOT_NAME_SENTINEL, expected_name)`
pass. The `ROOT_NAME_SENTINEL` value is a transport detail — it
never appears in committed `_generated_/` output.

Any sentinel must satisfy two constraints:

- **Survive byte-identically** through all three backends (no
  case-folding, no underscore stripping, no acronym up/down-casing).
  CamelCase identifiers do; `__ROOT__`-shape identifiers do not
  (`jtd-codegen` v0.4.1 normalizes underscore-prefixed identifiers,
  collapsing `__ROOT__` to `Root` across all three backends).
- **Be distinct enough that no real schema would emit it.**
  `StreamlibCanonRoot` is internally namespaced; a `s/Streamlib/`
  grep finds the implementation rather than user code.

The sub-type prefix-strip pass in Rust's `post_process_rust` runs
**before** the final sentinel substitution and operates on
`ROOT_NAME_SENTINEL`-prefixed names (e.g., `StreamlibCanonRootRateControl`
→ `RateControl`). Names that don't get stripped retain their
sentinel prefix until the final `code.replace`, at which point
`StreamlibCanonRootBar` becomes `EscalateRequestBar` — the existing
committed shape. Sub-type renames don't use a separate sentinel; the
prefix-strip heuristic operates on `ROOT_NAME_SENTINEL`-prefixed
input, which the upstream sentinel pass guarantees.

The two sentinel passes — cross-package `__STREAMLIB_REF_<hash>__`
and `ROOT_NAME_SENTINEL` — operate on disjoint parts of the input
(JTD `ref:` lines vs. the `--root-name` CLI arg), at disjoint
layers (pre-codegen JSON manipulation vs. post-codegen string
substitution), and never interact. Both implement the same
architectural principle (no heuristic detection at the codegen
boundary) at different surfaces.

## Anti-patterns

These are explicit rejections — re-introducing any of them
re-introduces the drift mode the design exists to eliminate.

### 1. `Identifier::parse(&str) -> Self` (or any equivalent)

There is no public `parse` constructor on `SchemaIdent`, `Org`,
`Package`, `TypeName`, or any future identifier type. A joined
identifier string never travels back through a parser at the
structured boundary.

The two allowed construction pathways:

- **Codegen-emitted construction** — `SchemaIdent::new(Org::new("tatolab").unwrap(), …)`
  lands in the macro-generated processor module at build time, exposed via
  a `pub fn schema_ident() -> SchemaIdent` on each `#[streamlib::processor("Camera")]`-
  decorated module. The function form (rather than a `const`) is forced
  by `SchemaIdent`'s validating constructors — `Org::new` / `Package::new`
  / `TypeName::new` aren't `const fn`. The function call is fully
  resolved at codegen and reads as a single line at every call site.
- **Typed YAML / JSON deserialization** — each segment is its own
  field in the source document; `serde` reads the structured shape
  directly into `SchemaIdent { org, package, r#type, version }`.

The compile-time witness is a set of `compile_fail` doctests on each
public identifier type (`SchemaIdent`, `Org`, `Package`, `TypeName`)
in `streamlib-idents` — the doctests assert the forbidden snippets
MUST fail to compile. If a `parse` method (or `FromStr` impl) is
ever added, the doctests would compile cleanly, the `compile_fail`
assertion flips, and `cargo test --doc -p streamlib-idents` surfaces
the regression. Each type locks both `Type::parse(...)` and
`"…".parse::<Type>()` so adding either entry point trips the gate.
The `tests/no_parse_api.rs` integration test is the positive
counterpart: it locks the *allowed* construction pathways
(validating `Type::new` constructors, typed YAML/JSON deserialization,
and explicitly that the joined Display form does NOT round-trip
back through deserialization). If you find yourself wanting a
parse method — even for a "tiny" helper, even "just for tests" —
stop. The drift starts that way.

### 2. Cross-package import-then-shorthand

In Rust source, this is the failure mode:

```rust
// ❌ WRONG — package-internal short name "leaks" cross-package
use tatolab_core::VIDEO_FRAME_IDENT;
graph.add_edge(VIDEO_FRAME_IDENT, …);   // No org/package on the wire
```

> ~~The package-internal short-name pattern (`#[streamlib::processor("Camera")]` — positional
> PascalCase short name resolved against the enclosing `streamlib.yaml`'s `package:` block) is the
> canonical shorthand for **owning** a processor's identity.~~ — Superseded 2026-07-19: a
> `#[processor("@org/package/Type")]` declares a **version-free identity in code** (or synthesizes
> `@app/local/<Type>`), reading nothing from a manifest. See
> [`zero-ceremony-authoring.md`](zero-ceremony-authoring.md).

Three macros **reference** a processor at a call site (typically the spawning binary that doesn't
own the processor's Rust module):

- **`streamlib::sdk::processor_type_ref!("org", "package", "Type")`**
  — the default reference form for the no-load-call world. Validates
  `(org, package, type)` at compile time and expands to a **version-free**
  `ProcessorTypeReference::ResolveToInstalled` value with **no package-source
  lookup at the call site**. Passed to `ProcessorSpec::new`, it reaches
  `add_processor`'s lazy hook and resolves to the single installed
  provider — loading its package from `streamlib_modules/` on first
  reference. This is what app code uses: no `add_module`, no version at
  the reference site. Every example uses it.
- **`streamlib::sdk::schema_ident_any_version!("org", "package", "Type")`**
  — the power-caller form. Resolves a `SchemaIdent` *now* against the
  already-registered processor types (highest registered `SemVer`,
  Cargo / npm convention), returning
  `Result<SchemaIdent, streamlib::sdk::error::Error>`. Reach for it only
  when the provider is already registered (a post-`add_module` /
  explicit-load call site) and you need the resolved `SchemaIdent`
  eagerly; otherwise prefer `processor_type_ref!`.
- **`streamlib::sdk::schema_ident!("org", "package", "Type", "1.0.0")`**
  — strict-pin reference form. Same four fields as the long
  `SchemaIdent::new(...)` constructor, validated at proc-macro
  expansion. Reach for it only when the call site has a deliberate
  reason to refuse newer-but-compatible versions.

Cross-package references in graph JSON, IPC envelopes, generated
code, and lockfiles still carry a fully-qualified
`SchemaIdent { org, package, type, version }` structured record. The
macro-emitted `schema_ident()` returns the structured record;
consumers can read its fields, but serializing across a wire surface
always emits the full structured shape.

### 3. Per-schema `version` field

Schema files declare `type` and content fields. They do **not**
declare a version. The CI lint catches re-introductions; the
architecture rejects them by design.

Why: per-schema versions create N-dimensional compatibility matrices
that consumers don't actually reason about. Publication-unit-scoped
versions collapse this to a single dimension per package.

### 4. Legacy metadata blocks in language-native manifests

`Cargo.toml` does not contain `[package.metadata.streamlib]`,
`pyproject.toml` does not contain `[tool.streamlib]`, and `deno.json`
has no `streamlib` block. The single source of truth is
`streamlib.yaml` for every runtime; the resolver feeds the resolved
set into each language's codegen pipeline. A CI lint
(`cargo xtask check-no-streamlib-metadata`) rejects re-introductions.

### 5. Hand-curated `embedded_schemas.rs`-style match statements

The schema registry at
`runtime/streamlib-engine/src/core/embedded_schemas/mod.rs` is a runtime
`LazyLock<RwLock<HashMap<String, Arc<str>>>>` populated by
`Runner::add_module`: for every package the project depends on,
the module loader walks the package's `schemas:` declarations and calls
`register_schema(canonical_id, yaml_body)`. Adding a schema means
declaring it in your `streamlib.yaml` and depending on the owning
package; nothing else. Hand-curated match arms mapping schema IDs to
embedded YAML strings are not allowed — they silently drift when a
schema renames or is added.

## Polyglot SchemaIdent parity

The `streamlib-idents` crate's full surface (range matching,
lockfile read/write, content-hash resolver, codegen pipeline)
is Rust-only. The Python SDK carries a focused subset matched
to the authoring path:

- **`streamlib.SchemaIdent`** — frozen dataclass mirroring the
  Rust 4-field shape with the same regex-validating constructors
  (org / package / type / version follow the same grammar that
  `streamlib_idents::Org` / `Package` / `TypeName` / `SemVer`
  enforce). No `parse` / `from_str` API; the joined `__str__` form
  is render-only and never round-trips through a parser.
- > ~~**`streamlib._manifest`** — hand-rolled YAML reader for the `package:` block + processor-name
  > list; **`@streamlib.processor("PascalCase")`** — positional short name resolved at decoration
  > time against the enclosing `streamlib.yaml`, validated against the manifest's `processors:` list.~~
  > — Superseded 2026-07-19: `_manifest` was removed and `@processor("@org/package/Type")` declares a
  > version-free identity from the decorator arguments, reading nothing from disk. See
  > [`zero-ceremony-authoring.md`](zero-ceremony-authoring.md).
- **`@streamlib.input(schema=...)` / `@streamlib.output(schema=...)`**
  — accept a `SchemaIdent` instance or a class that carries
  `__streamlib_schema_ident__`. Bare-string and joined-string
  forms are rejected at decoration time, mirroring the no-parse
  invariant on the Rust side.
- ~~**Schemas enter Python only through codegen.** Authors import generated
  dataclasses; there is no language-side affordance for declaring a schema —
  JTD-in-YAML is the canonical source and generated code is what authors import.~~
  — Superseded 2026-07-19 by the two-door descriptor model
  ([`zero-ceremony-authoring.md`](zero-ceremony-authoring.md)): the self-describing
  `Bag` wire carries its own field names, so no schema and no generated type is
  needed to interoperate; a by-ID JTD descriptor is consumed as data, never via
  required codegen. `@input(schema=GeneratedClass)` is now an optional typed view,
  not the only door schemas enter a language.

The reason for the focused subset rather than full parity:
structured-everywhere eliminates the need for non-Rust callers to
*validate identifiers* at runtime. Polyglot SDKs consume already-
validated records produced by Rust codegen or inbound IPC. The
Python SDK's local validators run only at authoring time —
guarding against manifest-vs-decorator drift, not validating
wire-format input.

Range matching, lockfile resolution, and the codegen pipeline
stay Rust-side because no non-Rust caller currently exercises
them. This matches the polyglot rule's escape clause
(`.claude/rules/polyglot.md`): *"the only legitimate split is
schema-only / language-specific by construction"* — the deeper
crate functionality (range matching, lockfile, codegen) is
"language-specific by construction" while basic identity
validation is mirrored across runtimes that need it.

### Codegen-emitted `SCHEMA_IDENT`

Decision 2 above lists "codegen-emitted const records
(`SCHEMA_IDENT: SchemaIdent { … }`)" as a structured-everywhere
surface. To the best of our current knowledge, the Python
post-processor in `streamlib-jtd-codegen` is the only one that
emits the structured ident on generated types
(`__streamlib_schema_ident__: ClassVar[SchemaIdent]`). The Rust
and TypeScript post-processors emit the dataclass / struct /
interface body but no `SCHEMA_IDENT` const. Python is the
load-bearing case because `@streamlib.input(schema=...)` /
`@streamlib.output(schema=...)` resolves the structured ident
*off the class* via `__streamlib_schema_ident__`; Rust and TS
have no analogous runtime resolution path that requires the
const today.

## Reference

- **Implementation**:
  - `sdk/streamlib-idents/` — `SchemaIdent`, `SemVer`, `SemVerRange`,
    `Manifest`, `PackageMetadata`, `Lockfile`. The `streamlib.yaml`
    resolver (`resolve`, `resolve_with`, `ResolvedPackages`) lives here
    too and walks path / git / `.slpkg` sources; the lockfile writer
    (`write_lockfile`, `read_lockfile`) and `compute_content_hash`
    helper are siblings of the resolver.
  - `sdk/streamlib-jtd-codegen/` — three-pass codegen pipeline.
    `sentinel.rs` substitutes cross-package refs with deterministic
    sentinels and restores them as native imports; `ordering.rs`
    stable-sorts every JSON object key before invoking `jtd-codegen`.
    Public entry `generate(GenerateOptions { project_dir, ... })`
    drives `streamlib.yaml`-mode end-to-end; `generate_from_resolved`
    is the lower-level entry for callers that already ran the
    resolver.
  - `runtime/streamlib-engine/src/core/embedded_schemas/mod.rs` — runtime
    `LazyLock<RwLock<HashMap<…>>>` registry; `register_schema` /
    `get_embedded_schema_definition` / `list_embedded_schema_names`
    public surface. Populated by `Runner::add_module` walking each
    loaded package's `schemas:` declarations.
  - `xtask/src/check_schema_versions.rs` — CI lint (no per-schema
    `version` keys in YAML).
  - `xtask/src/check_no_streamlib_metadata.rs` — CI lint
    (no `[package.metadata.streamlib]`, no `[tool.streamlib]`, no
    top-level `streamlib` key in `deno.json` / `deno.jsonc`).
  - `.github/workflows/check-schema-versions.yml` — schema-version CI gate.
  - `.github/workflows/check-no-streamlib-metadata.yml` —
    legacy-metadata CI gate.
  - `sdk/streamlib-python/python/streamlib/schema_ident.py` —
    Python `SchemaIdent` dataclass with regex-validating
    constructors and render-only joined `__str__`.
  - ~~`sdk/streamlib-python/python/streamlib/_manifest.py`~~ — removed 2026-07-19 (the decorators no
    longer read a manifest).
  - `sdk/streamlib-python/python/streamlib/decorators.py` —
    `@processor("@org/package/Type")` / `@input` / `@output`
    decorators; version-free identity from the decorator arguments,
    read from code, not a manifest.
- **Tests**:
  - `sdk/streamlib-idents/src/{ident,semver,manifest,lockfile,resolver}.rs::tests`
    — unit tests covering grammar conformance, semver-range matching,
    typed deserialization, lockfile round-trip + diff stability,
    content-hash determinism, and resolver scenarios (path / `.slpkg`
    / transitive / diamond / id-mismatch / registry-not-implemented).
  - `sdk/streamlib-idents/src/ident.rs` — `compile_fail` doctests on
    each identifier type that lock the no-`parse`-API invariant.
  - `sdk/streamlib-idents/tests/no_parse_api.rs` — positive
    counterpart: locks the *allowed* construction pathways and
    asserts joined-string deserialization fails.
  - `sdk/streamlib-jtd-codegen/src/{sentinel,ordering}.rs::tests`
    — pre-pass / post-pass coverage for sentinel substitution,
    deterministic property ordering, and per-language restore
    (Rust / Python / TypeScript).
  - `runtime/streamlib-engine/src/core/embedded_schemas/mod.rs::tests` —
    register / lookup round-trip, version-suffix stripping, empty-
    registry behavior, sorted listing, no duplicate names.
  - `xtask/src/check_schema_versions.rs::tests` — schema-version
    lint fixtures.
  - `xtask/src/check_no_streamlib_metadata.rs::tests` —
    legacy-metadata lint fixtures.
  - `sdk/streamlib-python/python/streamlib/tests/test_processor_decorator.py`
    — `SchemaIdent` validation, `@processor` version-free identity
    decoration, `@input` / `@output` schema rejection of
    bare-string and joined-string forms.
  - ~~`test_manifest_reader.py`~~ — removed 2026-07-19 with the `_manifest` reader.
- **Sibling architecture docs**:
  - [`compute-kernel.md`](compute-kernel.md), [`graphics-kernel.md`](graphics-kernel.md),
    [`ray-tracing-kernel.md`](ray-tracing-kernel.md) — the kernel-shape
    doc family.
  - [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) — the
    polyglot capability split this surface fits alongside.
  - [`texture-registration.md`](texture-registration.md) — engine-wide
    record pattern (`TextureRegistration`) that mirrors the same
    "single canonical record per concern" shape this surface applies
    to identifiers.
