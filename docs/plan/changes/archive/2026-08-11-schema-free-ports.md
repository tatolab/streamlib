# Change: schema-free-ports

**Change 1 of 3 from the 2026-08-03 align.** Siblings: `processor-class-identity.md`
(blocked on this one — `SchemaIdent` must lose its port role before it can lose its
identity role) and `one-monotonic-clock.md` (independent). Implements the four
`[schema-free-ports]` DECIDED entries in §Processor model & scheduling that concern the
type layer; the identity entries belong to Change 2. ADR:
`docs/decisions/schema-free-ports.md`.

Scale tier: change artifact + ADR — this touches the processor model **and** the IPC wire
format. ADR already written by the align.

Recon verified at HEAD `7d334ff7` on 2026-08-03 by four read-only sweeps (schema/JTD/
registry, wire/IPC/port surfaces, identity, clock).

## Supersedes `schema-agreement-ripout.md` — archive it, do not ship it

That change kept three things this decision deletes: unilateral port hints, an advisory
connect warn, and an inert-but-stamped wire tag. Its state at HEAD:

- Its connect-time half **already shipped** (commit `0113ea20`). `ConnectOptions`,
  `SchemaValidationPosture`, `connect_with{,_async}`, `enforce_connect_schema_agreement`,
  `classify_port_schema_agreement`, and `Error::SchemaIdentMismatch` are already gone.
- The advisory warn it proposed as MODIFIED (`port_schema_hints_differ`) was **never
  built** — there is nothing to remove.
- Its per-read half is fully live and is absorbed below.

Tickets #1679 and #1655 are superseded — #1679's stated constraint "`FrameHeader` BYTE
LAYOUT UNTOUCHED" is now false. `/reconcile-tracker` retires both; derive nothing from
that file.

## Sequencing

Land **after** the ripout's contract deletion (#1715). Two reasons, both from recon:
the 204→76 header change is a hard cross-runtime break that must move every publisher
and subscriber in one release, and while `sdk/streamlib-deno-native` and
`sdk/streamlib-python-native` still exist that means editing two cdylibs the ripout is
about to delete. Sequencing after it drops the break to two movers: the host and the
wheel's in-process path.

This change also **reverses two lines of `importable-python-library-ripout.md`**, which
must be amended in the same PR that lands this file: its `:43` preserves "the schema-ident
core behind the JTD seam" and demotes `streamlib-jtd-codegen` to internal-only. Both now
die outright (the ident core in Change 2, the codegen crate here).

## Behavior after this change

A port declares name, description, and — on an input — delivery profile. Nothing carries
a type. Connect wires without inspecting anything (already true). A frame header is 76
bytes and carries no schema ident, so no read path can compare one. A consumer discovers
a mismatch as a decode failure at its own read: in Rust serde is the always-on
validation, in Python `ctx.inputs.read(port)` yields the bag as a mapping and
`read(port, into=T)` is the opt-in strictness dial. `graph` and `tap` render a port as
name, description, delivery profile, direction.

Every input port declares its delivery profile explicitly; there is no default and
nothing left to infer one from.

## Two factual gaps, resolved by reading (not owner decisions)

- **`expected_payload_bytes` is safe to delete.** It is initial-allocation priming only,
  not correctness: the read path already grows on demand and stashes the oversized frame
  across the retry (`iceoryx2/input.rs:705-735` — "nothing is dropped", retiring the
  pre-#1421 up-front sizing). Only two engine-tree schemas set it. Cost: encoded-video
  channels start at the 64 KiB default and grow once instead of being primed at 4 MiB.
- **The `@session` isolation residue dissolves on its own.** `IsolationTier::for_processor`
  (`core/context/isolation.rs:93`) reaches the untrusted branch only when
  `cdylib_resident` is true; with the plugin ABI deleted that argument is always false and
  the tier collapses to a constant. That is the ripout's consequence, not this change's.

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: SchemaIdentWire
  The 128-byte wire tag whole: struct (`ipc-types/src/lib.rs:350`), `from_segments`,
  all five accessors, `render_joined`, `Debug`, `is_unset`, `matches_schema_tuple`, the
  three size constants, the compile-time ABI lock (`:364-369`), and seven unit tests
  (`:743-812`, `:861-903`). Includes the hand-written ctypes mirror in
  `sdk/streamlib-python/python/streamlib/frame_payload.py:53-143`.
- REMOVED: schema_agreement
  `runtime/streamlib-engine/src/core/schema_agreement.rs` whole (109 lines incl. its
  three revert-lock tests) and its declaration at `core/mod.rs:18`.
- REMOVED: classify_wire_schema_agreement
- REMOVED: expected_schema_ident
  `PortConfig.expected_schema_ident` (`iceoryx2/input.rs:221`), its init (`:274`), and
  `set_port_expected_schema_ident` (`:286`, sole non-test caller
  `open_iceoryx2_service_op.rs:692`).
- REMOVED: schema_mismatch_observed
  The `AtomicBool` (`input.rs:227`), the accessor (`:299`, no production consumer), the
  `read_raw_bounded` comparison block (`:466-484`), and the three per-read mismatch tests.
- REMOVED: schema_ident_wire_for_spec
- REMOVED: PortSchemaSpec
  The port type declaration itself (`streamlib-processor-schema/src/processor_schema.rs:42`),
  `PortDescriptor.schema`, the `port_schema_spec_wire` serde mirror, and
  `port_schema_spec_from_declaration` in the wheel.
- REMOVED: flow_class
  `FlowClass` (`iceoryx2/delivery_profile.rs:102-141`), `flow_class_for_port_spec`, and
  the `metadata.flow_class` declarations in the engine-tree schemas.
- REMOVED: embedded_schemas
  `runtime/streamlib-engine/src/core/embedded_schemas/` whole (721 lines + integration
  tests) — the process-wide schema registry, `delivery_profile_for_input_port`,
  `expected_payload_bytes_for_port_spec`, `port_schema_spec`, `resolve_node_port_schema`,
  and the `streamlib::schemas::*` public surface (`engine/src/lib.rs:41-54`).
- REMOVED: streamlib-jtd-codegen
  The crate whole, `engine/build.rs`'s `run_for_rust_crate`, the engine's `_generated_`
  shim module, `cargo xtask generate-schemas`, and the `jtd-codegen v0.4.1` install step
  in `.github/workflows/schemas.yml`.
- REMOVED: __streamlib_schema_ident__
  The generated-class marker and the `_generated_/` trees that carry it.
- REMOVED: check-processor-spec-new
  The xtask check and its workflow — it polices `SchemaIdent` literals in port specs.

## MODIFIED

- MODIFIED: `FRAME_HEADER_SIZE` 204 → **76** (`64 + 8 + 4`) and its two asserts
  (`ipc-types/src/lib.rs:210`, `:832-837`); new offsets `port_key 0..64`,
  `timestamp_ns 64..72`, `len 72..76`, `data 76..`. `FrameHeader::new` /
  `write_to_slice` / `read_from_slice` drop the ident; `FrameHeader::schema()` dies.
  Slot-priming arithmetic shifts by 128 bytes/frame (`iceoryx2/node.rs:168`,
  `output.rs:183`).
- MODIFIED: `set_channel_publisher` (`iceoryx2/output.rs:161`), `ChannelEgress`,
  `wire_rust_source` and `wire_rust_dest` (`open_iceoryx2_service_op.rs:639`, `:674`) —
  all drop their schema parameters; the per-frame stamp at `output.rs:284` drops the tag.
- MODIFIED: the host→subprocess wiring envelope JSON loses its `"schema"` key
  (`open_iceoryx2_service_op.rs:731`, helper `:40-48`) and its parsers stop reading it.
- MODIFIED: `delivery_profile` becomes **required on every input port** in all authoring
  surfaces — Rust `#[processor]` `input(...)`, the wheel's `@input`. It is optional today
  and defaults through the deleted `flow_class` chain. Every engine-tree input port gains
  an explicit declaration; a missing one is a wiring error.
- MODIFIED: the `schema=` kwarg is deleted from `@input`/`@output` in the wheel
  (`_processor_declaration.py:46-156`) and the positional `<schema>` from the Rust port
  grammar; `streamlib/schema_ident.py` and its `__init__.py` re-export die with it.
- MODIFIED: `PortInfo.data_type` (`graph/nodes/port_info.rs:13`) is deleted, and with it
  `PortInfoOutput.data_type` (`json_schema.rs:82`), `PortDescriptorOutput.schema`
  (`:222`), `RegisteredPortReceipt.schema` (`operations.rs:118`) and their four rendering
  tests. A port renders as `{name, description, port_kind, delivery_profile}`.
- MODIFIED: `sdk/streamlib-processor-schema` loses its schema half; `ExecutionConfig`,
  `ProcessExecution`, `ThreadPriority`, `ProcessorScheduling` and the non-schema half of
  `PortDescriptor` need a surviving home (crate layout final at implementation).

## ADDED

- ADDED: `read(port, into=T)` on the Python reader — the opt-in strictness dial. A
  TypedDict casts for free; a dataclass or pydantic model constructs and validates,
  raising at read. Attaches at `python_processor_context.rs:874` and
  `python_processor_link_data_access.rs:64`, with the matching `_engine.pyi:175` entry
  (the stub is part of done — stubtest + pyright gate it).
- ADDED: cast-not-contract conformance test — a producer publishing type X into a
  consumer declaring type Y delivers the bag; the consumer's `into=` read raises at read;
  the plain mapping read of the same frame succeeds; link and both processors stay healthy.
- ADDED: a missing-delivery-profile wiring-error test on an input port.

## Notes (not tickets)

- **The ship gate is a no-op for most existing REMOVED bullets.** It runs
  `git grep -InF` on everything after `- REMOVED:`, so any bullet carrying backticks or
  prose searches for that whole string and matches nothing. Verified: the gate passes
  `schema-agreement-ripout.md` clean while `schema_agreement` still exists at three
  sites. Bullets here are bare for that reason; the gate itself is worth a one-line fix.
- **The gate greps `packages/` and `examples/` too.** Those trees still carry schema
  idents and lag by design, so these REMOVED patterns cannot go green until the ripout
  and #1672 have moved them out. That is a sequencing fact, not extra scope.
