# Schema identity & packaging

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects design state as of 2026-05-04 (milestone 10 cleanup pass).
> Promoted out of issue #143's body to live as a tracked architecture
> doc alongside `texture-registration.md`, `compute-kernel.md`, etc.
> Original tracker issue closed 2026-05-04; this doc is now the
> canonical anchor.

## What this is

streamlib's schema identity and packaging architecture replaces three
half-implemented manifest formats, two competing identifier styles,
and a hand-curated `embedded_schemas.rs` lookup map with one
canonical shape:

- **One identifier grammar**: `@org/package/Type@version`, stored on
  the wire as a structured `SchemaIdent { org, package, type, version }`
  record — never a parsed string on the hot path.
- **One package manifest**: `streamlib.yaml` at the root of every
  package, declaring `org`, `package`, `version`, exports, and
  dependencies. The single source of truth.
- **One project manifest**: `streamlib.yaml` at the consumer
  project's root, declaring only `[project]` + `[dependencies]`.
- **One lockfile**: `streamlib.lock`, content-hashed, deterministic.
- **One CLI**: `streamlib generate`, `streamlib install`,
  `streamlib verify-schemas`. `cargo xtask` stays as a contributor
  shortcut.
- **One canonical wire vocabulary**: `@tatolab/core` ships
  `VideoFrame`, `AudioFrame`, `EncodedVideoFrame`, `EncodedAudioFrame`
  — the streamlib equivalent of `google.protobuf`'s well-known
  types.
- **One CI gate**: `streamlib verify-schemas` regenerates into a temp
  dir on every PR and diffs against committed `_generated_/`. Any
  drift fails the build.

## Why it exists

Before this architecture, schema metadata lived in
`[package.metadata.streamlib]` in `Cargo.toml`, an empty
`[tool.streamlib]` in `pyproject.toml`, nowhere at all in
`deno.json`, and 33 hardcoded `"com.tatolab.X"` string literals
scattered across 20+ example files. Schemas regenerated to
*different* output each run because jtd-codegen mangles names
inconsistently across backends (acronym upcasing in Python only,
digit-boundary lowercasing everywhere, the post-processor's
root-rename pass not firing on type-alias paths). Field ordering was
non-deterministic. `embedded_schemas.rs` was a hand-curated 120-line
match statement that drifted whenever someone forgot to update it.
The polyglot SDK had been bleeding 3-4 silent fix-PRs per quarter
(#383, #388, #389, #397) just keeping the runtimes in sync.

The engine-model fix — per
[CLAUDE.md "Engine-wide bugs get fixed at the engine layer"](../../CLAUDE.md#core-operating-principles--read-first)
— is a single canonical primitive (the manifest + identifier +
lockfile + sentinel-substitution-codegen pipeline) that every
runtime, package, and consumer flows through. No parallel
abstractions, no per-language drift surface, no implicit
convention substituting for type-system enforcement.

## The identifier grammar

```
@<org>/<package>/<Type>@<version>
```

Where:
- `<org>` and `<package>` are lowercase ASCII (`[a-z][a-z0-9-]*`),
  matching npm scope-and-package conventions.
- `<Type>` is strict PascalCase (`[A-Z][A-Za-z0-9]*`). Underscores
  forbidden. `H264DecoderConfig` is valid; `h264_decoder_config` is
  not.
- `<version>` is a full semver (`MAJOR.MINOR.PATCH`) at codegen
  time; semver ranges (`^1.0`, `~1.2.3`, etc.) are accepted at
  *dependency-declaration* time only.

### Where each piece comes from — package-as-publication-unit

The architecture follows the standard publication-unit pattern shared
by npm, Cargo, Maven, and Go modules: **`org`, `package`, and
`version` live on the package manifest; `Type` lives on individual
schema files; the full identifier is composed at codegen time**.

**`streamlib.yaml`** (package manifest at `packages/<name>/`):

```yaml
[package]
org = "tatolab"
name = "core"
version = "1.0.0"

[exports]
schemas = ["./schemas/*.yaml"]

[dependencies]
# (none for @tatolab/core; carve-out packages depend on it)
```

**`packages/core/schemas/AudioFrame.yaml`** (schema file):

```yaml
# AudioFrame.yaml — declares only the type's JTD shape
# org, package, version are inherited from packages/core/streamlib.yaml
properties:
  pts_us: { type: int64 }
  data:   { type: string }   # base64-encoded PCM
  channels: { type: uint16 }
  sample_rate: { type: uint32 }
```

Note what the schema file does **not** declare:
- No `org` field
- No `name` field
- No `version` field

These come from the enclosing package manifest. CI lint rejects any
schema YAML with a top-level `version` key.

**Codegen output** (`packages/core/_generated_/rust/audio_frame.rs`):

```rust
pub struct AudioFrame { /* ... */ }

impl AudioFrame {
    pub const SCHEMA_IDENT: SchemaIdent = SchemaIdent {
        org: "tatolab",
        package: "core",
        ty: "AudioFrame",
        version: SemVer { major: 1, minor: 0, patch: 0 },
    };
}
```

The `SCHEMA_IDENT` const is the canonical structured form. Every
type in `@tatolab/core@1.0.0` carries the same `org`, `package`, and
`version` triple — only `ty` varies. Bumping `@tatolab/core` from
`1.0.0` to `2.0.0` bumps the version on *every* type in the package,
even if only one type's shape changed. That's the publication-unit
contract — same as npm, where bumping `lodash@4.17.21 → 4.17.22`
re-stamps every export at the new version.

## Wire format — structured everywhere, never parsed

A reference to a schema in any of the following places carries the
**full structured `SchemaIdent` record**, never a joined string:

- IPC envelopes (cross-runtime, cross-process)
- Generated code consts (`pub const SCHEMA_IDENT: SchemaIdent = …`)
- Build.rs-emitted env vars (4 separate vars, or one JSON-encoded)
- Example pipeline graph JSON
- The embedded-schema lookup map's keys
- The web UI's pipeline graph rendering and API server's responses

```json
// Graph JSON example — every reference is fully self-describing
{
  "processors": [
    {
      "name": "cam",
      "type": { "org": "tatolab", "package": "camera",
                "type": "LinuxCamera",
                "version": "1.0.0" },
      "outputs": [
        { "schema": { "org": "tatolab", "package": "core",
                      "type": "VideoFrame",
                      "version": "1.0.0" } }
      ]
    }
  ]
}
```

There is **no shorthand mechanism**. No `[imports]` block in graph
JSON that resolves bare names against, no aliases, no
context-dependent name resolution. The verbose form is the only
form. This is deliberate: AI agents, the web UI, the API server,
container builders, registries, and every other downstream consumer
must be able to read a single reference and know exactly what it
means without plumbing context elsewhere in the document. See the
[anti-patterns](#anti-patterns) section for the rejection rationale.

A `Display` impl renders the joined form (`@tatolab/core/AudioFrame@1.0.0`)
for human-readable diagnostics, logs, and error messages — but
**no consumer ever calls `parse`** to split it back into fields.
The `streamlib-idents` crate exposes `Display` and `SchemaIdent`
construction; it does **not** expose `parse` as a first-class API.

## Why structured everywhere — three reinforcing reasons

1. **Drift modes go away by construction.** The original problem
   statement is "schemas don't regenerate consistently across
   languages". A parser-based design produces a class of subtle bugs
   (parser disagreement on whitespace, Unicode normalisation, escape
   handling) that you mitigate with cross-language parity tests.
   A structured design has *no parser to disagree*. The drift mode
   is unreachable.
2. **Future tooling consumes structured data natively.** A web UI
   listing packages, a container builder pulling specific package
   versions, a registry server returning manifest metadata, an AI
   agent reasoning about a pipeline graph — none of them need to
   parse a streamlib-specific identifier grammar. They consume
   `{ org, package, type, version }` as JSON or whatever structured
   format their stack already speaks.
3. **Type-system enforcement beats convention** per CLAUDE.md's
   engine-model rule. A `SchemaIdent` struct is the type-system
   encoding of "an identifier is exactly four fields"; a string is
   convention saying "if you parse it correctly, you'll find four
   fields". Engine-tier primitives use the type system.

## The codegen pipeline

```
streamlib.yaml + schemas/*.yaml
   │
   ▼
streamlib-codegen resolver
   │   walks streamlib.yaml + lockfile
   │   resolves path / git / .slpkg deps
   │
   ▼
jtd-codegen with sentinel root-name
   │   --root-name __STREAMLIB_ROOT__
   │   per-backend: rust, python, typescript
   │
   ▼
post-processor: literal substitution
   │   s/__STREAMLIB_ROOT__/<Type>/g
   │   sub-types: __STREAMLIB_ROOT__Variant → <Type>Variant
   │   field-order normalization across all backends
   │   schema-ident structured-const emission
   │
   ▼
packages/<name>/_generated_/{rust,python,typescript}/
```

### The sentinel-substitution contract

jtd-codegen has known driver bugs:
- It ignores `--root-name` on some paths
- It applies digit-boundary lowercasing in all three backends
- It applies acronym upcasing in the Python backend only

The fix is the **sentinel-substitution contract**: pass jtd-codegen
a literal sentinel root name (`__STREAMLIB_ROOT__`) that contains no
acronyms to upcase, no digit boundaries to lowercase, and no
constructs that trip the per-backend mangling. Whatever jtd-codegen
emits, the post-processor replaces with the literal `<Type>` from
`streamlib.yaml` after the fact via `s/__STREAMLIB_ROOT__/<Type>/g`.
The sentinel sidesteps all three driver bugs by construction. No
acronym heuristics, no digit-boundary rules, no detection of
whether the root is a struct/enum/type-alias — none of that
matters because the post-processor does *literal* replacement.

Sub-type names follow the same pattern: jtd-codegen emits
`__STREAMLIB_ROOT__Variant`; post-processor strips the sentinel
prefix and emits `<Type>Variant`.

### Field-ordering normalisation

The sentinel pattern handles type *naming* drift. Field *ordering*
drift is its own concern — jtd-codegen's per-backend struct emission
can iterate the JTD `properties` map in non-deterministic order. The
post-processor normalises this with a deterministic field-ordering
pass: every emitted struct has its fields sorted lexicographically
by JTD property name across all three backends. Same input → same
output, every regeneration, every backend.

## CI gate (`streamlib verify-schemas`)

Every PR runs `streamlib verify-schemas`, which:

1. Walks `streamlib.yaml` + `streamlib.lock` from each package
2. Regenerates all bindings into a fresh temp dir
3. Diffs against the committed `_generated_/` directories
4. Diffs the lockfile against the resolver's expected output
5. Lints for forbidden sections (`[package.metadata.streamlib]`,
   `[tool.streamlib]`, etc.)
6. Lints schema YAML files for rogue `version` keys (per the
   package-as-publication-unit rule)
7. Exits non-zero on any drift

This catches naming drift, ordering drift, lockfile staleness, and
manifest-format violations on every PR — before the divergence ships.

## Universal CLI (`streamlib generate`)

`streamlib-codegen` is extracted into a reusable Rust crate, and
`streamlib generate` is a first-class CLI subcommand. The binary
ships prebuilt for every supported platform (Linux x86_64, Linux
aarch64, macOS x86_64, macOS aarch64). Non-Rust developers regenerate
bindings without ever installing rustup; Python and Deno teams own
their bindings without depending on a Rust toolchain.

`cargo xtask generate-schemas` remains as a contributor shortcut
that wraps the same library, but is no longer the only path to
codegen.

## Anti-patterns

These are the failure modes the engine-model rule exists to prevent.
Each was either tried and rejected during the milestone-10 design
discussion (2026-05-04), or is the foreseeable workaround a future
agent might attempt without this doc.

1. **Per-schema `version` field.** Schemas inside a package do
   *not* declare their own version. `streamlib.yaml` is the only
   place a `version` field lives. The publication unit is the
   package; bumping any type bumps the whole package. CI lint
   rejects any schema YAML with a top-level `version` key. If you
   want independent versioning of a single type, that type belongs
   in its own package.
2. **`Identifier::parse` on the hot path.** The `streamlib-idents`
   crate has no public `parse` API. `Display` exists for
   diagnostics; `SchemaIdent` is constructed via codegen or via
   typed deserialisation of YAML / JSON. If you find yourself
   wanting to split a string at runtime, you're re-introducing the
   parser-disagreement drift mode this design eliminated.
3. **Cross-package import-then-shorthand.** No `[imports]` block in
   graph JSON that resolves bare names against the project manifest.
   No aliases. Every reference in a wire format is a fully-qualified
   structured record. *Why this was rejected:* AI-friendliness is a
   design goal of streamlib; shorthand introduces cross-agent
   variance because two agents reading similar references with
   different enclosing-context imports can derive different
   fully-qualified meanings. Web UI and API server consumers
   shouldn't have to plumb context to render or process a single
   reference. The intra-package short-name macro
   (`@streamlib.processor("AudioMixer")` resolving via the enclosing
   `streamlib.yaml`) is a *narrow, intra-package* exception that
   does not generalise to cross-package wire formats.
4. **Manifest metadata in `Cargo.toml` / `pyproject.toml` / `deno.json`.**
   Once the resolver lands, these files contain *zero* streamlib
   metadata. Their native build systems still own them, but
   schema-related state lives in `streamlib.yaml` only. CI lint
   rejects re-introduction.
5. **Free-form identifier strings in processor source.** Macros
   take short names (`#[streamlib::processor("AudioMixer")]`); the
   full structured identifier is derived from the enclosing
   `streamlib.yaml` at build time and injected via codegen. There
   is no "just hardcode the full string" escape hatch — it
   re-introduces the drift the design eliminated.
6. **Reaching for jtd-codegen heuristics or per-backend customisation.**
   The post-processor does literal substitution and deterministic
   field ordering. Any "but Python wants snake_case here" or "but
   TypeScript wants camelCase there" instinct is a request to
   re-introduce the mangling-drift bug class. Same shape across all
   three backends, post-processor enforced.

## The `@tatolab/core` package

`@tatolab/core` is the canonical wire vocabulary — streamlib's
analogue of `google.protobuf`'s well-known types. It ships from day
one as `v1.0.0` (these types are stable by definition; breaking
changes require a deliberate v2 bump and downstream migration, not a
patch version) and contains:

- `VideoFrame`
- `AudioFrame`
- `EncodedVideoFrame`
- `EncodedAudioFrame`

Every other in-tree package depends on `@tatolab/core`. Every
external package author who ships processors against streamlib
depends on `@tatolab/core`. It's the wire ABI; everything else
composes on top.

## Per-package carve-outs

Beyond `@tatolab/core`, streamlib's processors and adapters carve
out into their own `@tatolab/<name>` packages — `audio`, `camera`,
`h264`, `h265`, `mp4`, `network`, `opengl`, `vulkan`, `skia`, `cuda`,
`polyglot-runtime`, etc. Each ships its own `streamlib.yaml`,
declares its own version, and is published as an independent
`.slpkg`. Consumers depend on the packages they need; the resolver
walks the dependency graph and pins everything in `streamlib.lock`.

The carve-outs land as filed sub-issues within milestone 10; each
one has the same shape (declare the package directory, migrate the
relevant schemas, update consumers, verify regen idempotency under
the CI gate).

## References

### Issues
- **#143** (closed 2026-05-04 — content moved here): original
  milestone tracker
- **#399**: identifier grammar + `streamlib-idents` validator crate
  spec
- **#400**: extract `streamlib-codegen` crate + `streamlib generate`
  CLI
- **#401**: `@tatolab/core` package + well-known wire-type migration
- **#402**: resolver + lockfile + manifest-cutover (absorbs #403,
  #405)
- **#404**: processor short-name macros (absorbs #407 example
  string sweep)
- **#406**: CI verify-schemas gate
- **#408**: re-enable disabled examples on the new processor API
- **#541**: empty-struct/type-alias idempotency fix
- **(NEW)**: deterministic field-ordering normalisation across
  backends

### Closed-as-superseded
- **#116**, **#117** (closed 2026-05-04): legacy plugin manifest
  drafts; the registry/signing concerns will return as their own
  future milestones once the foundation here ships.

### Related
- [adapter-runtime-integration.md](adapter-runtime-integration.md) —
  how subprocess customers obtain a usable adapter context
- [subprocess-rhi-parity.md](subprocess-rhi-parity.md) —
  single-pattern principle for surface adapters
- [docs/issue-template.md](../issue-template.md) — issue-shape rules
  the milestone follows
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md) —
  the polyglot rule that governs schema regen across runtimes

### External references
- npm: package-as-publication-unit pattern (`@scope/package@version`)
- Cargo: `[package]` + lockfile pattern this design parallels
- JSON Type Definition (RFC 8927) — the schema language
- jtd-codegen — the per-language codegen used under the
  sentinel-substitution layer
