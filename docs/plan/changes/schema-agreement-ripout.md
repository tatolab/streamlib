# Change: schema-agreement-ripout

> **Superseded 2026-08-03 by `schema-free-ports.md`** (align #1723, ADR
> `docs/decisions/schema-free-ports.md`). This change was built around three things the
> decision deletes: unilateral port hints, an advisory connect warn, and an inert-but-
> stamped wire tag. Its connect-time half already shipped (commit `0113ea20`); the
> advisory warn it proposed was never built; its per-read half is absorbed by
> `schema-free-ports.md`. Its central constraint — "`FrameHeader`/`SchemaIdentWire` byte
> layout is unchanged" — is now false: the header goes 204 → 76 bytes. Do not derive
> tickets from this file; `/reconcile-tracker` retires #1679 and #1655.

> **Reconciliation note 2026-08-02** (`importable-python-library` pivot): the core
> decision here — advisory connect, cast-at-read, inert wire tag — stands unchanged.
> But this file's STAY rationale cites `module_loader/` paths, `sdk/streamlib-deno`,
> the consumer-upgrade-backlog doctrine, and plugin-ABI version bumps — all deleted by
> the pivot. Where a cited path is gone, the pivot's change files govern; the
> `SchemaIdent` survival case is registry/factory only.

Implements `[data-plane-cast-not-contract]` (§Processor model & scheduling) and the
"bags/schemas fixed" clause of the zero-ceremony bar (§Product). ADR:
`docs/decisions/data-plane-cast-not-contract.md`. Recon verified every claim below at
file:line on 2026-07-29; the machinery has exactly one connect-time comparison site, one
per-read comparison site, one `SchemaIdentMismatch` construction, and zero production
callers that select strict.

## Behavior after this change

Connect always wires. When both ends declare concrete, tuple-distinct schema hints, the
engine emits one advisory `warn` (the plan's "advisory log at most") and wires anyway.
No read path compares wire tags. The 128-byte `SchemaIdentWire` keeps its byte layout
and stamping (producers still write it) but no code makes a decision from it — it is
observability metadata for tap, logs, and catalog rendering. Version fields are
write-only diagnostics. Consumers discover type mismatches the decided way: the cast at
read fails (`BagDecodeFailed` / typed-view error) at the consuming processor.

## REMOVED

Each bullet is a pattern the ship gate verifies is gone: **one artifact per bullet, plain
text, on the bullet's first line.** Continuation lines are prose the gate does not search.

> ~~Each bullet is a grep pattern the ship gate verifies is gone (scoped path in
> parens).~~ — Superseded 2026-08-08 by PR #1788. The gate has no per-bullet path scope,
> and a backticked or parenthesised pattern is now rejected rather than searched. All
> nine bullets below were backticked, so none of them ever searched for anything that
> appears in Rust source.
>
> **Five of the nine already landed.** PR #1687 deleted the strict/loose posture ahead of
> this change: `SchemaValidationPosture`, `ConnectOptions`, `connect_with` /
> `connect_with_async`, `enforce_connect_schema_agreement`,
> `classify_port_schema_agreement`, `SchemaIdentMismatch`,
> `connect_schema_agreement_tests` and
> `app_connect_with_forwards_strict_posture_to_runner` are all at zero in the tree. The
> file:line citations below predate that PR — `schema_agreement.rs` is 109 lines now, not
> the 386 the first bullet claims. Left as written: re-deriving the inventory is this
> change's substance, not this grammar repair. Noted 2026-08-08.

- REMOVED: schema_agreement
- REMOVED: runtime/streamlib-engine/src/core/schema_agreement.rs

  The whole module (386 lines incl. its 10 unit tests) and every reference; declaration
  `core/mod.rs:18`. The symbol bullet proves nothing references it; the path bullet proves
  the file is gone.
- REMOVED: SchemaValidationPosture

  Defined `schema_agreement.rs:48`; re-exports `operations.rs:20`,
  `core/runtime/mod.rs:31`, `lib.rs:89`.
- REMOVED: ConnectOptions

  Defined `operations.rs:33` — its only field is the posture, so the type goes;
  re-exports `lib.rs:66`, `core/runtime/mod.rs:29`.
- REMOVED: connect_with(
- REMOVED: connect_with_async(

  `operations_runtime.rs:564,:592`; `sdk/streamlib-sdk/src/sdk/app.rs:111` — plain
  `connect`/`connect_async` remain the only wiring surface.

  > ~~`connect_with`~~ — Corrected 2026-08-08 to `connect_with(`. The bare symbol matches
  > the surviving test names `connect_with_unknown_source_processor_id_…` and
  > `connect_with_unknown_target_processor_id_…` in
  > `runtime/streamlib-engine/tests/connect_typed_errors_test.rs`, which are unrelated and
  > are not removed, so it could never reach zero. The trailing paren matches the
  > definition and every call site, and neither test name.
- REMOVED: enforce_connect_schema_agreement
- REMOVED: classify_wire_schema_agreement
- REMOVED: classify_port_schema_agreement

  All `schema_agreement.rs`; call sites `operations_runtime.rs:249`,
  `iceoryx2/input.rs:467`.
- REMOVED: expected_schema_ident
- REMOVED: schema_mismatch_observed
- REMOVED: set_port_expected_schema_ident

  The two `PortConfig` fields (`iceoryx2/input.rs:221,:227`),
  `set_port_expected_schema_ident` (`input.rs:286`; sole non-test caller
  `open_iceoryx2_service_op.rs:692`), the accessor (`input.rs:299`, no production
  consumer), and the `read_raw_bounded` comparison block (`input.rs:467-484`).
- REMOVED: SchemaIdentMismatch

  The error variant (`sdk/streamlib-error/src/lib.rs:111-123`; sole construction
  `schema_agreement.rs:152`; all five `matches!` arms are test assertions).
- REMOVED: is_unset
- REMOVED: matches_schema_tuple

  `is_unset` and `matches_schema_tuple` on `SchemaIdentWire`
  (`runtime/streamlib-ipc-types/src/lib.rs:430,:455`) — their only callers are the
  agreement module. `SchemaIdent::matches_schema_tuple` / `schema_identity_tuple` in
  `sdk/streamlib-idents` STAY (registry lookup, module-loader hot-swap and staging
  paths: `processor_instance_factory.rs:1352`, `module_loader/mod.rs:636,:648,:1100`,
  `staging.rs:151`).

  **[NEEDS DECISION]** `matches_schema_tuple` cannot reach zero: this bullet's own prose
  keeps `SchemaIdent::matches_schema_tuple` in `sdk/streamlib-idents/src/ident.rs`, and
  the surviving method carries the same name as the dying one, so no textual pattern
  separates them. Options: (a) rename the surviving `SchemaIdent` method so the dying
  `SchemaIdentWire` name is unique — the zero-context naming rule arguably wants that
  regardless; (b) drop the bullet and let the `SchemaIdentWire` removal ride the
  `is_unset` bullet plus review. Recommendation: (a). Owner call; not resolved here.
- REMOVED: connect_schema_agreement_tests
- REMOVED: app_connect_with_forwards_strict_posture_to_runner
- REMOVED: read_raw_observes_schema_tag_mismatch_but_still_delivers
- REMOVED: read_raw_is_silent_on_matching_or_wildcard_schema
- REMOVED: read_raw_is_silent_across_the_version_free_sentinel_asymmetry

  The agreement test suites — `connect_schema_agreement_tests`
  (`operations_runtime.rs:632-1050`, incl. its test-only registry scaffolding),
  `app_connect_with_forwards_strict_posture_to_runner`
  (`sdk/streamlib-sdk/tests/app_sugar_test.rs:166,:207`), and the three per-read mismatch
  tests, now named one per bullet above (`iceoryx2/input.rs:950,:977,:1005` — the
  citations still resolve exactly).

  > ~~and the three per-read mismatch tests~~ / ~~+ helper~~ — Corrected 2026-08-08.
  > The three tests were named only by line number in prose, so no bullet searched for
  > them and all three are live: exactly the defect this repair exists to remove,
  > reproduced inside it. They are bulleted above. The unnamed `+ helper` is dropped
  > rather than guessed: `app_connect_with_forwards_strict_posture_to_runner` and its
  > helper are already at zero (deleted by #1687 — `app_sugar_test.rs` survives, the
  > symbols do not), so there is no name left in the tree to write.

## MODIFIED

- MODIFIED: `connect_impl` (`operations_runtime.rs:171`) — drops the `validation`
  parameter and the enforcement block (`:236-260`); in their place, one advisory
  `tracing::warn!` when both resolved `PortSchemaSpec`s are concrete and tuple-distinct
  (small private helper in `operations_runtime.rs`, e.g.
  `port_schema_hints_differ(&PortSchemaSpec, &PortSchemaSpec) -> bool` — zero-context
  name final at implementation). Wildcards (`Any`/unset) never warn; versions never
  participate.
- MODIFIED: `wire_rust_dest` (`open_iceoryx2_service_op.rs:674`) — stops resolving and
  feeding `dest_schema` for expectation; output-side stamping
  (`wire_rust_source`/`:657`, `output.rs:284`) is unchanged. The subprocess dest path
  already carries no schema (`:775-782`) — after this change the Rust host matches it.
- MODIFIED: `runtime/streamlib-ipc-types/src/lib.rs` — doc on `SchemaIdentWire`
  (`:323-349`) rewritten: the tag is write-only observability metadata; version fields
  are diagnostics; the #1460/#1477 `==`-trap paragraph replaced by "nothing may compare
  these" language. Byte layout, `from_segments`, slice encode/decode, `render_joined`,
  `Debug` all stay (layout locks untouched).
- MODIFIED: stale doc promises that referenced the removed check — `input.rs:210-213`
  (`staged_oversized`) and `:447` (`read_raw_bounded` doc), plus the posture prose in
  `operations.rs:28-50`, `operations_runtime.rs:164,:563,:637`, `app.rs:109`.
- MODIFIED: `docs/decisions/data-plane-cast-not-contract.md` — consequences section
  gains one line naming this change as the implementing delta.

## ADDED

- ADDED: advisory-connect test — connecting two concrete, tuple-distinct hints wires
  the link AND emits exactly one warn (replaces `loose_connect_warns_but_wires_…` with
  posture machinery gone); a wildcard pairing stays silent.
- ADDED: cast-not-contract conformance test — a producer publishing type X into a
  consumer declaring type Y delivers the bag; the consumer's typed read fails with the
  cast error at read time, the link and both processors stay healthy, and the raw
  `Bag` read of the same frame succeeds.
- ADDED: wire-tag inertness guard — an xtask-greppable invariant or unit test asserting
  no code path outside rendering reads `SchemaIdentWire` fields for control flow
  (shape final at implementation; the ship gate's REMOVED greps already cover the known
  sites).

## Out of scope (adjacent, already tracked)

- #1655 (delete runtime schema registry / delivery-profile move) — separate ticket.
- #1662 (GraphSnapshot::validate version-inclusive lookup) — separate bug ticket.
- Any `packages/`/`examples/` caller using `ConnectOptions::strict()` — consumer
  upgrade backlog by doctrine, not this change.
- `sdk/streamlib-deno/schema_ident.ts` `equals()` — authoring surface, no read-path
  caller; untouched.

## Wire/ABI note

`FrameHeader`/`SchemaIdentWire` byte layout is unchanged (204/128 bytes, layout locks
stay), so no plugin-ABI version bump and no cross-language wire migration. The change
is engine-tree only; the polyglot natives already have no expected-schema path.
