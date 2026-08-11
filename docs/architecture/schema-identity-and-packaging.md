# Schema identity & packaging

> Current known state of the schema-identity surface. Subject to
> staleness or drift — verify against the code before relying on any
> claim. Not authoritative, not enforcement.

## What this document describes

The identifier grammar the `streamlib-idents` crate covers, and the
anti-patterns the design rules out.

> ~~package manifest formats, dependency resolver, lockfile~~ — Superseded
> 2026-08-11: the package layer is deleted. `streamlib.yaml`, the resolver,
> the lockfile, the package source and the archive readers no longer exist,
> and nothing in the tree reads a manifest. The sections describing them are
> struck below. What remains here — the grammar, the structured-everywhere
> rule, and anti-patterns 1 and 2 — is live until `processor-class-identity`
> replaces identity with the class's import path, which deletes this document
> along with the crate.

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
- **Incomplete distribution attempts** (a hand-curated match statement
  in the engine, ad-hoc `.slpkg` archive experiments, schemas that
  lived only in `runtime/streamlib-engine/schemas/` with no publication
  story).

The fix is one cohesive architecture covering identifier grammar,
package manifest, dependency resolution, and distribution.

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
- Graph JSON (the runtime's serialized pipeline graph).

> ~~Lockfile entries (`streamlib-codegen.lock`).~~ — Superseded 2026-08-11:
> the lockfile is deleted, so it is no longer a surface this rule covers.

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

> Removed 2026-08-11: `streamlib.yaml` is deleted, so there is no
> publication unit to scope a version to, and the `check-schema-versions`
> lint that enforced it is gone with it. Versions never live at the code
> layer; distribution versioning is a wheel/crate concern.

### Decision 4 — `streamlib-codegen.lock` for content-hash resolution

> Removed 2026-08-11: the lockfile, the resolver that wrote it, and the
> content-hash machinery are deleted. PyPI and cargo are the package
> systems, and nothing is resolved or downloaded at runtime.

### Decision 5 — `@tatolab/core` is the canonical wire vocabulary

The four wire-stable types every other package depends on
(`VideoFrame`, `AudioFrame`, `EncodedVideoFrame`, `EncodedAudioFrame`)
live in a single `@tatolab/core` package at `packages/core/`. This is
streamlib's `google.protobuf` analogue. `@tatolab/core` ships at
`1.0.0` from day one; breaking changes require a deliberate v2 bump
and downstream migration.

## Manifest formats

> Removed 2026-08-11: both `streamlib.yaml` flavors, the three dependency
> source kinds (version / path / git), and the semver-range operator table
> that served them. `Manifest`, `PackageMetadata`, `DependencySpec` and the
> resolver are deleted. A StreamLib app is a normal Python or cargo codebase
> and declares nothing to streamlib. `SemVerRange` itself outlives this
> section but no longer has a manifest to range over.

## Lockfile shape

> Removed 2026-08-11: `streamlib-codegen.lock`, `streamlib.lock`, their
> entry shape and the content-hash contract. Nothing resolves a package set,
> so nothing pins one.

## Crate ownership

The `streamlib-idents` crate owns the structured identifier types
(`SchemaIdent`, `Org`, `Package`, `TypeName`, `PackageRef`, `ModuleIdent`),
`SemVer`, and the channel-name grammar.

> ~~and the resolver that walks `streamlib.yaml` (`resolve`,
> `ResolvedPackages`) […] so one `streamlib.yaml` carries both the
> schema-identity surface and the runtime configuration.~~ — Superseded
> 2026-08-11: `Manifest`, `PackageMetadata`, `Lockfile` and the resolver are
> deleted from the crate.

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
> the zero-ceremony authoring model.

Three macros **reference** a processor at a call site (typically the spawning binary that doesn't
own the processor's Rust module):

- **`streamlib::sdk::processor_type_ref!("org", "package", "Type")`**
  — the default reference form for the no-load-call world. Validates
  `(org, package, type)` at compile time and expands to a **version-free**
  `ProcessorTypeReference::ResolveToInstalled` value with **no package-source
  lookup at the call site**. Passed to `ProcessorSpec::new`, it reaches
  `add_processor`'s lazy hook and resolves to the single installed
  provider. This is what app code uses: no version at the reference site.

  > ~~loading its package from `streamlib_modules/` on first reference~~,
  > ~~no `add_module`~~ — Superseded 2026-08-11: the module loader and
  > `streamlib_modules/` are deleted. `ProcessorTypeReference` and this
  > macro family retire with the identity grammar in
  > `processor-class-identity`.
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

> Removed 2026-08-11: there is no schema layer to carry a version and no
> package to scope one to. The `check-schema-versions` lint is deleted.

### 4. Legacy metadata blocks in language-native manifests

> Removed 2026-08-11: the rule survives as plan doctrine — a StreamLib app
> declares nothing to streamlib in its language-native manifest
> (`docs/plan/ARCHITECTURE.md` §Product, the zero-ceremony bar) — but the
> remedy this section named, "put it in `streamlib.yaml`", no longer exists,
> and the `check-no-streamlib-metadata` lint that enforced it is deleted.

## Polyglot SchemaIdent parity

The `streamlib-idents` crate's full surface (grammar validation, semver
range matching) is Rust-only. The Python SDK carries a focused subset
matched to the authoring path:

- > ~~**`streamlib._manifest`** — hand-rolled YAML reader for the `package:` block + processor-name
  > list; **`@streamlib.processor("PascalCase")`** — positional short name resolved at decoration
  > time against the enclosing `streamlib.yaml`, validated against the manifest's `processors:` list.~~
  > — Superseded 2026-07-19: `_manifest` was removed and `@processor("@org/package/Type")` declares a
  > version-free identity from the decorator arguments, reading nothing from disk. See
  > the zero-ceremony authoring model.
- **`@streamlib.input` / `@streamlib.output`** — a port declares name,
  description and, on an input, a delivery profile. The port method's return
  annotation is the type declaration, read by humans and type checkers only.
- **No schema and no generated type is needed to interoperate.** The
  self-describing `Bag` wire carries its own field names, and a by-ID JTD
  descriptor is consumed as data. The opt-in typed read is
  `ctx.inputs.read(port, into=T)`.

The reason for the focused subset rather than full parity:
structured-everywhere eliminates the need for non-Rust callers to
*validate identifiers* at runtime. Polyglot SDKs consume already-
validated records produced by Rust or inbound IPC. The
Python SDK's local validators run only at authoring time —
guarding against manifest-vs-decorator drift, not validating
wire-format input.

Range matching stays Rust-side because no non-Rust caller currently
exercises it. This matches the polyglot rule's escape clause
(`.claude/rules/polyglot.md`): *"the only legitimate split is
schema-only / language-specific by construction"* — range matching is
"language-specific by construction" while basic identity
validation is mirrored across runtimes that need it.

## Reference

- **Implementation**:
  - `sdk/streamlib-idents/` — `SchemaIdent`, `SemVer`, `SemVerRange`,
    and the channel-name grammar.
  - `sdk/streamlib-python-wheel/python/streamlib/_processor_declaration.py`
    — the authoring decorators; they carry no `SchemaIdent`.
- **Tests**:
  - `sdk/streamlib-idents/src/{ident,semver,channel}.rs::tests`
    — unit tests covering grammar conformance, semver-range matching,
    and typed deserialization.
  - `sdk/streamlib-idents/src/ident.rs` — `compile_fail` doctests on
    each identifier type that lock the no-`parse`-API invariant.
  - `sdk/streamlib-idents/tests/no_parse_api.rs` — positive
    counterpart: locks the *allowed* construction pathways and
    asserts joined-string deserialization fails.
  - `sdk/streamlib-python-wheel/tests/test_processor_declaration.py` —
    `@processor` version-free identity decoration, delivery-profile
    enforcement, and the locks that a port declaration takes no `schema=`
    and carries no type key under any spelling.
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
