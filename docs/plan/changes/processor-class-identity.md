# Change: processor-class-identity

**Change 2 of 3 from the 2026-08-03 align.** Blocked on `schema-free-ports.md`:
`SchemaIdent` must lose its port-type role before it can lose its identity role, and
`sdk/streamlib-idents/src/ident.rs` only becomes deletable once both are gone. Implements
the two `[schema-free-ports]` DECIDED entries about identity and display name in
§Processor model & scheduling. ADR: `docs/decisions/schema-free-ports.md`.

Scale tier: change artifact + ADR — it changes the processor model's identity contract
and the control plane's `type` field. ADR already written by the align.

Recon verified at HEAD `7d334ff7` on 2026-08-03.

## Behavior after this change

A processor is identified by its class's fully-qualified import path —
`my_app.filters:BlurProcessor` in Python, `my_app::filters::BlurProcessor` in Rust.
Derived mechanically, never authored. One string in the registry, in the control plane's
`type` field, and for helper-process placement. `@processor` declares execution,
interval, scheduling priority, and description only.

An instance's display name is the human-facing label — passed at `add`, readable off the
returned handle, prefixing its log records, defaulting to the class's short name, with
the engine disambiguating duplicates within one graph.

## What recon changed about the plan's own claims

- **`display_name` already exists end to end** and needs no new surface: the wheel's
  `rt.add(cls, *, config, display_name)` (`python_runtime_lifecycle.rs:169-176`), the
  `AddedProcessor` handle (`python_added_processor.rs:13-63`), `ProcessorNode.display_name`
  (`processor_node.rs:21`), the log-record prefix (`python_processor_host.rs:268-277`),
  and the control-plane field (`json_schema.rs:47`). The only missing piece of that plan
  entry is the **duplicate disambiguation**, which does not exist: `add_v_op.rs:59-61`
  assigns the type short name verbatim, so `rt.add(Blur)` twice yields two nodes both
  labeled `Blur`. The only counter in the tree is snapshot-time and applies to the alias,
  not the display name (`runtime.rs:1313-1328`).
- **The ADR's "helper-process placement already requires the import path" is half-true.**
  Helper spawn genuinely consumes a `module:Type` string
  (`spawn_python_native_subprocess_op.rs:132` → `subprocess_runner.py:95-99`), but nothing
  derives it *from the class object*: it is authored in `streamlib.yaml`
  (`processor_registration.rs:447`) or computed from the staged file path
  (`from_source.rs:335-344`). Both derivation sites die with the module loader. So class-path
  identity is **new derivation code, not reuse**. `__module__` + `__qualname__` are already
  read at `python_processor_registration.rs:90-100`, but only to format an error.
- **Rust identity is captured by the macro, not `std::any::type_name`.** `type_name`'s
  output format is explicitly unspecified across compiler versions and must not key a
  registry. The `#[processor]` macro captures the type path at the expansion site.
  Resolved by reading — the plan says "derived mechanically" and this is the only stable
  mechanism.

  > ~~The macro captures `concat!(module_path!(), "::", stringify!(Type))`.~~ — Superseded
  > 2026-08-11 by #1839. The macro wraps its whole expansion in `pub mod <AuthoredType>`
  > (`codegen.rs` binds the module name to `item.ident`), so the descriptor is emitted
  > *inside* a module already named for the author's type: a bare `module_path!()` is the
  > full type path, and the form above would double the type name. Asserted as literals in
  > `runtime/streamlib-engine/tests/processor_class_import_path_test.rs`.

## Processors defined in `__main__` — REVERSED by owner, 2026-08-04

The 2026-08-03 ruling ("Option (a): legal, in-process only") is withdrawn by the
helper-placement pivot (`docs/decisions/helper-process-placement-only.md`): in-process
hosting of a Python processor is banned, so a `__main__`-defined class has no legal
host anywhere. A `@processor` class defined in the entry file registers as
`__main__:<Type>` and is a **wiring error at `rt.add`**, with an actionable error
naming the fix (move the class to an importable module and import it from the entry
file — one import line), because a helper importing `__main__` gets its own entry
file, not the user's.

The reversal does not retire the `python app.py` harness — the *app* may still run as
`__main__`; only *processor classes* may not live there. The scaffold puts the effect
class in its own importable module beside `app.py`. Identity stops being
per-launch-arrangement: a legal processor always lives in an importable module and
always has the same name.

Still rejected, same reasons: making `streamlib run`/`dev` the only launcher
(contradicts "an importable Python library" and retires the harness the lifecycle
contract was proven against), and rewriting `__main__` to a path-derived module name
(path-dependent identity is the exact failure the ADR names).

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: streamlib-idents
  The crate whole. `ident.rs` (`Org`, `Package`, `TypeName`, `PackageRef`, `SchemaIdent`,
  `ModuleIdent`, the `validate_*` grammar, the no-parse compile-fail gates), `semver.rs`
  (`SemVer` loses its last consumer with the package layer), `session.rs`. `channel.rs`
  (`ChannelName`, `source_channel_name`) is the one non-ident survivor and needs a home —
  it is live at `operations_runtime.rs:21` and `open_iceoryx2_service_op.rs:353`.
- REMOVED: SchemaIdent
- REMOVED: SchemaIdentOutput
  The control-plane DTO (`schema_ident_output.rs:42`) and the `ProcessorNodeOutput.processor_type`
  object shape (`json_schema.rs:44-46`) — `type` becomes a plain string.
- REMOVED: ProcessorTypeReference
  The version-free narrowing (`processor_type_reference.rs:30`), its three-key JSON
  wire lock (`processor_spec.rs:76-110`), and `to_diagnostic_ident`.
- REMOVED: schema_ident_any_version
- REMOVED: resolve_installed_processor_type
  With `highest_registered_for_tuple`, `resolve_any_version`, `schema_identity_tuple` and
  `matches_schema_tuple` — version-blind lookup has nothing left to be blind about.
- REMOVED: @app/local
  The synthesis at `_processor_declaration.py:314` and `grammar.rs:255-278`.
- REMOVED: is_reserved_for_session
- REMOVED: session_isolation_tier
  Recorded 2026-08-12 during #1840, which found the scope wider than "loses its `Org`
  parameter". `IsolationTier::for_processor` read `(org, cdylib_resident)`, and
  `is_cdylib_resident` has returned a literal `false` since the plugin ABI died — so with
  the org gone the derivation has exactly one reachable answer and collapses to a
  constant. Its operator knob collapses with it: `set_session_isolation_tier`,
  `SESSION_TIER_OVERRIDE` and the `STREAMLIB_SESSION_ISOLATION_TIER` env var only ever
  answered "is this module `@session`?". `IsolationTier` itself, its `Untrusted` variant
  and the `FullAccessGrant` moat survive — the moat is a compile-time guarantee about who
  may mint an in-process `RuntimeContextFullAccess`, not a placement question, and the
  seam an untrusted-code path returns through must be rebuilt against the helper-process
  boundary rather than against an org.
- REMOVED: IsolationTier::for_processor
- REMOVED: check-no-reverse-dns
  The xtask check and its workflow. Its stated rationale is enforcing the
  `@org/package/Type@version` grammar (`check_no_reverse_dns.rs:5-9`); with that grammar
  deleted it has no rule left to enforce. This supersedes the ripout's "re-scope" line.
- REMOVED: check-schema-versions
  The xtask check and its workflow. Added 2026-08-11 during #1837, which deleted both:
  the importable-python-library ripout's 2026-08-09 supersession kept this gate and
  `check-no-streamlib-metadata` only until the manifest retired and named this change as
  where they go, but recorded it as prose in an archived file, which the ship gate cannot
  read. The gate enforced "versioning lives in `streamlib.yaml`, not in individual
  schemas" — both halves are now deleted concepts.
- REMOVED: check-no-streamlib-metadata
  The xtask check and its workflow, deleted alongside `check-schema-versions` under the
  same supersession. The ban it enforced survives as plan doctrine — a StreamLib app
  declares nothing to streamlib in its language-native manifest (§Product, the
  zero-ceremony bar) — but the remedy it named, `streamlib.yaml`, no longer exists.

## MODIFIED

- MODIFIED: registry and factory keys move from `SchemaIdent` to the import-path string —
  the three maps at `processor_instance_factory.rs:807-809`, `ProcessorNode.processor_type`
  (`processor_node.rs:20`), `GraphSnapshot`'s `ProcessorDefinition.processor_type`
  (`graph_snapshot.rs:79`), the pubsub events (`events.rs:270-326`), the wheel's class
  cache (`python_processor_registration.rs:26`) and its duplicate-identity error (`:57-62`,
  which today tells the author to declare an explicit `@org/package/Type`).
- MODIFIED: `@processor` loses its identity argument in both languages — the positional
  identity (`_processor_declaration.py:179-195`, `grammar.rs:161-167`), the
  `_IDENTITY_PATTERN` regex (`:33`), the `type =` override (`grammar.rs:217`), and the
  `VERSION_FREE_SENTINEL` (`python_processor_declaration.rs:23`). Execution, interval,
  scheduling and description stay.
- MODIFIED: `App::add_local::<P>` (`sdk/streamlib-sdk/src/sdk/app.rs:62-89`) drops its
  `@session/…` ident minting and registers under the captured type path. Rust `App::add`
  gains a display-name parameter and returns a handle rather than a bare
  `ProcessorUniqueId` — today it can reach neither (`ProcessorSpec::with_display_name`
  exists but `add` never calls it).
- MODIFIED: `sdk/streamlib-macros` loses the `schema_ident!` / `schema_ident_any_version!`
  / `module_ident*!` family and gains the type-path capture.
- MODIFIED: `runtime/streamlib-moq/src/moq_catalog.rs:24-57` keys on the import path.

## ADDED

- ADDED: display-name disambiguation at `add_v_op.rs:59` — the engine appends a counter
  when a display name already exists in the graph. Mirrored or removed at
  `python_runtime_lifecycle.rs:198-199`, where the wheel pre-computes the default
  client-side and never round-trips it.
- ADDED: import-path derivation for Python (`__module__` + `__qualname__`, colon-joined)
  and the macro capture for Rust, each with a unit test.
- ADDED: the `__main__`-identity refusal at `rt.add` — an actionable error naming the
  fix (move the class to an importable module), with a test proving a `__main__`-defined
  class is refused and the same class in an importable module is accepted. (Amended
  2026-08-04: the original bullet mandated a test proving an in-process `__main__`
  processor runs — a banned capability under helper-only placement.)
- ADDED: an identity-stability test — a class in an importable module registers under the
  same string however the app was launched.

## Out of scope

- The `setup(rt)` entry-file loading glue — #1711 / `importable-python-library.md`. This
  change constrains it (the entry must be imported, not run) but does not build it.
- Re-authoring `packages/` and `examples/` off the old grammar — deferred re-authoring,
  recorded in the ripout's dispositions.
