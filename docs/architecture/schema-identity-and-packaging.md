# Schema identity & packaging

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects code state as of 2026-05-04 (PR for issue #399 — first
> deliverable of milestone 10, *Schema Identity & Packaging*). The full
> migration is staged across multiple PRs (#400, #401, #402, #404, plus
> the 12 carve-out package issues); claims about *current* code are
> point-in-time and stale fast.

## Status

This document is the canonical architecture brief for milestone 10. It
ships alongside the `streamlib-idents` Rust crate (the first code-level
expression of the design); future agents should read this doc for
design context and the [milestone-10 description][m10] for current
pickup ordering.

[m10]: https://github.com/tatolab/streamlib/milestone/10

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
  schemas that lived only in `libs/streamlib/schemas/` with no
  publication story).

The fix is one cohesive architecture covering identifier grammar,
package manifest, dependency resolution, code generation, and
distribution. The rest of milestone 10 implements it.

## Architectural decisions

These are load-bearing — relaxing any of them brings back the failure
mode that motivated the whole milestone.

### Decision 1 — `@org/package/Type@version` identifier grammar

Replaces `com.tatolab.videoframe@1.0.0` (reverse-DNS, lowercase, no
package boundary in the name) with `@tatolab/core/VideoFrame@1.0.0`
(npm-style scope, explicit package, PascalCase type name, semver).

The grammar (BNF):

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
- Lockfile entries (`streamlib.lock`).

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
YAML declaring a top-level `version` key. The wider lint that also
rejects `metadata.version` ships when the schema files are migrated
to the new shape (in #401 / #402).

### Decision 4 — `streamlib.lock` for content-hash resolution

Every project that consumes packages (applications, examples) writes
a `streamlib.lock` next to its `streamlib.yaml`. The lockfile is
content-hash-pinned and diff-stable (sorted `BTreeMap` keys), so a
fresh checkout reconstructs the same generated bindings byte-for-byte.

Discipline (mirrors `Cargo.lock`):

- **Commit `streamlib.lock`** in applications, examples, and any
  non-publishable consumer.
- **Don't commit `streamlib.lock`** in publishable libraries — they
  inherit their consumer's lock.

### Decision 5 — `@tatolab/core` is the canonical wire vocabulary

The four wire-stable types every other package depends on
(`VideoFrame`, `AudioFrame`, `EncodedVideoFrame`, `EncodedAudioFrame`)
live in a single `@tatolab/core` package. This is streamlib's
`google.protobuf` analogue. `@tatolab/core` ships at `1.0.0` from day
one; breaking changes require a deliberate v2 bump and downstream
migration.

Twelve carve-out packages live under `packages/` and depend on
`@tatolab/core`:

| Package | Owner of |
|---|---|
| `@tatolab/audio` | audio capture / output / mixer / channel-converter / resampler / chord-generator / buffer-rechunker |
| `@tatolab/camera` | camera capture |
| `@tatolab/display` | display / window / swapchain |
| `@tatolab/h264` | H.264 encoder + decoder |
| `@tatolab/h265` | H.265 encoder + decoder |
| `@tatolab/opus` | Opus encoder + decoder |
| `@tatolab/mp4` | MP4 writer (Apple + Linux variants) |
| `@tatolab/webrtc` | WHEP + WHIP |
| `@tatolab/moq` | MoQ publish + subscribe tracks |
| `@tatolab/api-server` | runtime API server |
| `@tatolab/clap` | CLAP audio plugin host |
| `@tatolab/screen-capture` | screen capture |

### Decision 6 — universal `streamlib generate` CLI

Code generation goes through `streamlib generate` (a subcommand on
the `streamlib` CLI). Non-Rust developers regenerate bindings without
ever installing rustup. `cargo xtask generate-schemas` remains as a
thin contributor-only wrapper.

The library crate behind both — `streamlib-codegen` — is what #400
extracts from the current `xtask/src/generate_schemas.rs`.

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

### Decision 8 — `streamlib verify-schemas` CI determinism gate

A CI gate (`streamlib verify-schemas`, wired on every PR) runs
`streamlib generate` and asserts the generated bindings are
byte-identical to what's checked in. If they aren't, the PR fails
and the author re-runs `streamlib generate` locally.

This is where the `streamlib.lock` content hash becomes load-bearing:
the gate proves that committed `_generated_/` matches the lockfile's
inputs.

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

- **Registry** (string form `"^1.0.0"` or `{ version: "^1.0.0" }`).
  Resolved against a registry; the v1 design assumes a Cloudflare R2
  / GitHub Releases-backed registry, but the resolver is source-
  agnostic.
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

`streamlib.lock` is the resolved-set companion to `streamlib.yaml`.
Wire shape:

```yaml
version: 1
packages:
  "@tatolab/core":
    version: 1.0.0
    source:
      kind: registry
      url: https://packages.streamlib.dev
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
| `source` | Discriminated union: `registry` / `path` / `git` (`tag = kind`, snake-case). |
| `content_hash` | Namespace-prefixed (`sha256:…`) so future hash-algorithm migrations don't break parsing. |

The content hash is computed over the resolved package's contents
(deterministic pass over schemas + manifest). This is what the
`verify-schemas` CI gate compares against — the gate catches both
"someone hand-edited generated code" and "someone bumped a dep
without re-locking."

## Sentinel-substitution codegen contract

Code generation runs in three passes:

1. **Resolve** — read `streamlib.yaml` + `streamlib.lock`, walk the
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
directory. The `verify-schemas` gate re-runs the whole pipeline on
CI and asserts byte-identical output.

The `streamlib-codegen` crate (extracted by #400) owns this pipeline.
The `streamlib-idents` crate (this PR) owns the structured types
the pipeline reads and writes (`SchemaIdent`, `SemVer`,
`PackageManifest`, `Lockfile`).

## Anti-patterns

These are explicit rejections — re-introducing any of them
re-introduces the drift mode the design exists to eliminate.

### 1. `Identifier::parse(&str) -> Self` (or any equivalent)

There is no public `parse` constructor on `SchemaIdent`, `Org`,
`Package`, `TypeName`, or any future identifier type. A joined
identifier string never travels back through a parser at the
structured boundary.

The two allowed construction pathways:

- **Codegen-emitted const literals** — `SCHEMA_IDENT: SchemaIdent =
  SchemaIdent::new(Org::new("tatolab").unwrap(), …)` lands in the
  generated `_generated_/` files at build time.
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

The package-internal short-name pattern (`#[streamlib::processor(name =
"Camera")]`) is the **only** shorthand mechanism in the architecture.
Cross-package references in graph JSON, IPC envelopes, generated
code, and lockfiles carry a fully-qualified `SchemaIdent { org,
package, type, version }` structured record.

When the macro emits per-package consts, those consts hold a
*structured* `SchemaIdent` — the consumer can read its fields, but
serializing across a wire surface always emits the full structured
record.

### 3. Per-schema `version` field

Schema files declare `type` and content fields. They do **not**
declare a version. The CI lint catches re-introductions; the
architecture rejects them by design.

Why: per-schema versions create N-dimensional compatibility matrices
that consumers don't actually reason about. Publication-unit-scoped
versions collapse this to a single dimension per package.

### 4. Legacy metadata blocks in language-native manifests

After #402 lands, `Cargo.toml` does not contain
`[package.metadata.streamlib]`, `pyproject.toml` does not contain
`[tool.streamlib]`, and `deno.json` has no `streamlib` block. The
single source of truth is `streamlib.yaml` for every runtime; the
resolver feeds the resolved set into each language's codegen
pipeline.

CI lint (added in #402) rejects re-introductions.

### 5. Hand-curated `embedded_schemas.rs`-style match statements

Pre-#402, `libs/streamlib/src/core/embedded_schemas.rs` had a
hand-curated `match` mapping schema IDs to embedded YAML strings.
Two failure modes: (a) easy to forget to add a new schema; (b)
silent drift when a schema renamed but the match arm didn't.

The replacement is resolver-driven: the resolved package set
populates a `SchemaIdent`-keyed lookup table at build time. Adding
a schema means declaring it in your `streamlib.yaml`; nothing else.

## Rosetta — current → new identifier mapping

Every current `com.streamlib.*` / `com.tatolab.*` identifier maps to
a `@org/package/Type@version` form below. The migration is staged:

- **Wire types** (`@tatolab/core`) migrate in **#404**.
- **Processor names + their config schemas** migrate in **#401**
  (the macro rewrite is what makes the short-name pattern available)
  and the carve-out package issues (one per package).
- **Polyglot escalate IPC** migrates in #404 alongside the wire-type
  migration (it's the same wire surface).

### Wire vocabulary → `@tatolab/core@1.0.0`

| Current | New |
|---|---|
| `com.tatolab.videoframe@1.0.0` | `@tatolab/core/VideoFrame@1.0.0` |
| `com.tatolab.audioframe@1.0.0` | `@tatolab/core/AudioFrame@1.0.0` |
| `com.tatolab.encodedvideoframe@1.0.0` | `@tatolab/core/EncodedVideoFrame@1.0.0` |
| `com.tatolab.encodedaudioframe@1.0.0` | `@tatolab/core/EncodedAudioFrame@1.0.0` |

### Audio package → `@tatolab/audio@1.0.0`

| Current processor / schema | New |
|---|---|
| `com.tatolab.audio_capture` | `@tatolab/audio/AudioCapture@1.0.0` |
| `com.tatolab.audio_capture.config@1.0.0` | `@tatolab/audio/AudioCaptureConfig@1.0.0` |
| `com.tatolab.audio_output` | `@tatolab/audio/AudioOutput@1.0.0` |
| `com.tatolab.audio_output.config@1.0.0` | `@tatolab/audio/AudioOutputConfig@1.0.0` |
| `com.tatolab.audio_mixer` | `@tatolab/audio/AudioMixer@1.0.0` |
| `com.tatolab.audio_mixer.config@1.0.0` | `@tatolab/audio/AudioMixerConfig@1.0.0` |
| `com.tatolab.audio_channel_converter` | `@tatolab/audio/AudioChannelConverter@1.0.0` |
| `com.tatolab.audio_channel_converter.config@1.0.0` | `@tatolab/audio/AudioChannelConverterConfig@1.0.0` |
| `com.tatolab.audio_resampler` | `@tatolab/audio/AudioResampler@1.0.0` |
| `com.tatolab.audio_resampler.config@1.0.0` | `@tatolab/audio/AudioResamplerConfig@1.0.0` |
| `com.tatolab.buffer_rechunker` | `@tatolab/audio/BufferRechunker@1.0.0` |
| `com.tatolab.buffer_rechunker.config@1.0.0` | `@tatolab/audio/BufferRechunkerConfig@1.0.0` |
| `com.tatolab.chord_generator` | `@tatolab/audio/ChordGenerator@1.0.0` |
| `com.tatolab.chord_generator.config@1.0.0` | `@tatolab/audio/ChordGeneratorConfig@1.0.0` |

### Camera package → `@tatolab/camera@1.0.0`

| Current | New |
|---|---|
| `com.tatolab.camera` | `@tatolab/camera/Camera@1.0.0` |
| `com.tatolab.camera.config@1.0.0` | `@tatolab/camera/CameraConfig@1.0.0` |

### Display package → `@tatolab/display@1.0.0`

| Current | New |
|---|---|
| `com.tatolab.display` | `@tatolab/display/Display@1.0.0` |
| `com.tatolab.display.config@1.0.0` | `@tatolab/display/DisplayConfig@1.0.0` |

### H.264 package → `@tatolab/h264@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.h264_encoder` | `@tatolab/h264/H264Encoder@1.0.0` |
| `com.streamlib.h264_encoder.config@1.0.0` | `@tatolab/h264/H264EncoderConfig@1.0.0` |
| `com.streamlib.h264_decoder` | `@tatolab/h264/H264Decoder@1.0.0` |
| `com.streamlib.h264_decoder.config@1.0.0` | `@tatolab/h264/H264DecoderConfig@1.0.0` |

### H.265 package → `@tatolab/h265@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.h265_encoder` | `@tatolab/h265/H265Encoder@1.0.0` |
| `com.streamlib.h265_encoder.config@1.0.0` | `@tatolab/h265/H265EncoderConfig@1.0.0` |
| `com.streamlib.h265_decoder` | `@tatolab/h265/H265Decoder@1.0.0` |
| `com.streamlib.h265_decoder.config@1.0.0` | `@tatolab/h265/H265DecoderConfig@1.0.0` |

### Opus package → `@tatolab/opus@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.opus_encoder` | `@tatolab/opus/OpusEncoder@1.0.0` |
| `com.streamlib.opus_encoder.config@1.0.0` | `@tatolab/opus/OpusEncoderConfig@1.0.0` |
| `com.streamlib.opus_decoder` | `@tatolab/opus/OpusDecoder@1.0.0` |
| `com.streamlib.opus_decoder.config@1.0.0` | `@tatolab/opus/OpusDecoderConfig@1.0.0` |

### MP4 package → `@tatolab/mp4@1.0.0`

| Current | New |
|---|---|
| `com.tatolab.mp4_writer` | `@tatolab/mp4/Mp4Writer@1.0.0` |
| `com.tatolab.mp4_writer.config@1.0.0` | `@tatolab/mp4/Mp4WriterConfig@1.0.0` |
| `com.streamlib.linux_mp4_writer` | `@tatolab/mp4/LinuxMp4Writer@1.0.0` |
| `com.streamlib.linux_mp4_writer.config@1.0.0` | `@tatolab/mp4/LinuxMp4WriterConfig@1.0.0` |

### WebRTC package → `@tatolab/webrtc@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.webrtc_whep` | `@tatolab/webrtc/WhepReceiver@1.0.0` |
| `com.streamlib.webrtc_whep.config@1.0.0` | `@tatolab/webrtc/WhepReceiverConfig@1.0.0` |
| `com.streamlib.webrtc_whip` | `@tatolab/webrtc/WhipSender@1.0.0` |
| `com.streamlib.webrtc_whip.config@1.0.0` | `@tatolab/webrtc/WhipSenderConfig@1.0.0` |

### MoQ package → `@tatolab/moq@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.moq_publish_track` | `@tatolab/moq/PublishTrack@1.0.0` |
| `com.streamlib.moq_publish_track.config@1.0.0` | `@tatolab/moq/PublishTrackConfig@1.0.0` |
| `com.streamlib.moq_subscribe_track` | `@tatolab/moq/SubscribeTrack@1.0.0` |
| `com.streamlib.moq_subscribe_track.config@1.0.0` | `@tatolab/moq/SubscribeTrackConfig@1.0.0` |

### API server package → `@tatolab/api-server@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.api_server` | `@tatolab/api-server/ApiServer@1.0.0` |
| `com.streamlib.api_server.config@1.0.0` | `@tatolab/api-server/ApiServerConfig@1.0.0` |

### CLAP package → `@tatolab/clap@1.0.0`

| Current | New |
|---|---|
| `com.streamlib.clap.effect` | `@tatolab/clap/ClapEffect@1.0.0` |
| `com.streamlib.clap.effect.config@1.0.0` | `@tatolab/clap/ClapEffectConfig@1.0.0` |

### Screen-capture package → `@tatolab/screen-capture@1.0.0`

| Current | New |
|---|---|
| `com.tatolab.screen_capture` | `@tatolab/screen-capture/ScreenCapture@1.0.0` |
| `com.tatolab.screen_capture.config@1.0.0` | `@tatolab/screen-capture/ScreenCaptureConfig@1.0.0` |

### Internal / not-yet-packaged

These don't fit one of the 12 carve-outs but exist in the current
schema set. They'll either land in their own carve-out package or
be folded into an existing one — TBD per the per-carve-out issues.

| Current | Likely destination |
|---|---|
| `com.streamlib.bgra_file_source` | `@tatolab/sources/BgraFileSource@1.0.0` (new package) |
| `com.streamlib.escalate_request@1.0.0` | `@streamlib/escalate/EscalateRequest@1.0.0` (escalate IPC; #404) |
| `com.streamlib.escalate_response@1.0.0` | `@streamlib/escalate/EscalateResponse@1.0.0` (#404) |
| `com.tatolab.simple_passthrough` | test-only — likely stays under a `@streamlib/test` namespace |
| `com.streamlib.test.*` | test-only — `@streamlib/test/...@0.1.0` (kept off the public registry) |

## Pickup order (multi-PR migration plan)

The architecture lands across many PRs. The dependency graph (per
GitHub `Blocked by` edges) drives the order:

1. **#541** + **#684** + **#399** — bug fixes (codegen idempotency,
   field ordering) and this foundation. Sibling-ready.
2. **#400** — extract `streamlib-codegen` crate + add
   `streamlib generate` CLI. Needs #399 for the structured types.
3. **#402** — `streamlib.yaml` resolver + `streamlib.lock` + cutover
   off legacy `[package.metadata.streamlib]` etc. (atomic — absorbs
   #403 and #405).
4. **#401** — processor short-name macros + sweep hardcoded
   reverse-DNS literals across Rust + Python + Deno.
5. **#404** — `@tatolab/core` package + IPC wire-format migration
   to structured records. First end-to-end dogfooding of the
   architecture.
6. **#406** + **#408** + the **12 carve-out packages** — the long
   tail. By this point the architecture is proven and the carve-outs
   are mechanical.

Each later issue is a planned consumer of this PR's foundation. The
"no bad patterns left behind on engine changes" rule (CLAUDE.md) is
explicitly relaxed across the milestone — the migration is staged
across PRs by design, not bandaided on top of one giant PR.

## Why no Python / Deno parity in v1

The `streamlib-idents` crate ships Rust-only. Python and Deno do
not get matching `SchemaIdent` validators / range matchers in v1 —
deliberately scope-cut.

The reason is structural, not ergonomic: **structured-everywhere
eliminates the need for non-Rust callers to validate identifiers.**
Python and Deno consume `SchemaIdent` records that have already been
validated upstream (by codegen at build time, or by the Rust host
on inbound IPC). No subprocess-side parser exists, because no
subprocess-side parsing happens.

If a future non-Rust caller actually needs to validate identifiers
locally (e.g. a Python tool authoring `streamlib.yaml` programmatically),
file a follow-up at that point. Building three parser-parity test
corpora upfront for hypothetical consumers is exactly the burden the
structured-everywhere decision exists to eliminate.

This is the same shape as the polyglot rule's escape clause
(`.claude/workflows/polyglot.md`): *"the only legitimate split is
schema-only / language-specific by construction"* — here the split is
"language-specific by construction," because the design eliminates
the cross-language need.

## Reference

- **Implementation**:
  - `libs/streamlib-idents/` — `SchemaIdent`, `SemVer`, `SemVerRange`,
    `PackageManifest`, `ProjectManifest`, `Lockfile`.
  - `xtask/src/check_schema_versions.rs` — CI lint.
  - `.github/workflows/check-schema-versions.yml` — CI gate.
- **Issue**: #399 — this PR.
- **Milestone**: [Schema Identity & Packaging (10)][m10] — freshness
  anchor for in-flight design.
- **Tests**:
  - `libs/streamlib-idents/src/{ident,semver,manifest,lockfile}.rs::tests`
    — unit tests covering grammar conformance, semver-range matching,
    typed deserialization, lockfile round-trip + diff stability.
  - `libs/streamlib-idents/src/ident.rs` — `compile_fail` doctests on
    each identifier type that lock the no-`parse`-API invariant
    (proven real: temporarily add `pub fn parse(_: &str) -> Self` to
    `SchemaIdent` and `cargo test --doc` reports
    "Test compiled successfully, but it's marked `compile_fail`").
  - `libs/streamlib-idents/tests/no_parse_api.rs` — positive
    counterpart: locks the *allowed* construction pathways and
    asserts joined-string deserialization fails.
  - `xtask/src/check_schema_versions.rs::tests` — fixture tests +
    workspace smoke test.
- **Future research / follow-ups**:
  - #400 (`streamlib-codegen` crate + CLI).
  - #401 (macros + example string migration).
  - #402 (resolver + lockfile + cutover off legacy metadata).
  - #404 (`@tatolab/core` + IPC wire migration).
  - 12 carve-out package issues.
- **Sibling architecture docs**:
  - [`compute-kernel.md`](compute-kernel.md), [`graphics-kernel.md`](graphics-kernel.md),
    [`ray-tracing-kernel.md`](ray-tracing-kernel.md) — the kernel-shape
    doc family.
  - [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) — the polyglot
    capability split this milestone fits alongside.
  - [`texture-registration.md`](texture-registration.md) — engine-wide
    record pattern (`TextureRegistration`) that mirrors the same
    "single canonical record per concern" shape this milestone applies
    to identifiers.
