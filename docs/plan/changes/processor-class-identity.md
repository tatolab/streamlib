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
  registry. The `#[processor]` macro captures
  `concat!(module_path!(), "::", stringify!(Type))` at the expansion site. Resolved by
  reading — the plan says "derived mechanically" and this is the only stable mechanism.

## Processors defined in `__main__` — RESOLVED by owner, 2026-08-03

**Option (a): legal, in-process only.** A `@processor` class defined in the entry file the
user runs with `python app.py` registers as `__main__:BlurProcessor` and runs in-process
like any other. Identity stays mechanical and honest — it is whatever the import system
says. The constraint lands at helper placement, where it actually bites: the engine
refuses to helper-place a `__main__`-defined class with an actionable error ("define it
in an importable module to run it in a helper process"), because a helper importing
`__main__` gets its own, not the user's entry file.

This keeps `python app.py` working, keeps the scaffold free to put a processor in the
entry file, and matches the DECIDED entry that a helper-placeable class "must be
import-addressable from a module whose import is side-effect-safe"
(ARCHITECTURE.md:104). The plan's identity entry is corrected accordingly — the
"never executed as `__main__`" clause is replaced.

Consequence to hold: the same class has a different identity under `python app.py`
(`__main__:Blur`) than under a loader that imports the entry (`app:Blur`). That is
accepted — identity is per-launch-arrangement, and the only thing that reads it across
processes is helper placement, which `__main__` classes are refused from anyway.

Rejected: making `streamlib run`/`dev` the only launcher (contradicts "an importable
Python library" and retires the harness the lifecycle contract was proven against), and
rewriting `__main__` to a path-derived module name (path-dependent identity is the exact
failure the ADR names).

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
- REMOVED: check-no-reverse-dns
  The xtask check and its workflow. Its stated rationale is enforcing the
  `@org/package/Type@version` grammar (`check_no_reverse_dns.rs:5-9`); with that grammar
  deleted it has no rule left to enforce. This supersedes the ripout's "re-scope" line.

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
- ADDED: the helper-placement refusal for `__main__`-defined classes — an actionable
  error naming the fix (move the class to an importable module), with a test proving an
  in-process `__main__` processor runs and the same class is refused a helper placement.
- ADDED: an identity-stability test — a class in an importable module registers under the
  same string however the app was launched.

## Out of scope

- The `setup(rt)` entry-file loading glue — #1711 / `importable-python-library.md`. This
  change constrains it (the entry must be imported, not run) but does not build it.
- Re-authoring `packages/` and `examples/` off the old grammar — deferred re-authoring,
  recorded in the ripout's dispositions.
