# Schema-free ports

Rationale for the `[schema-free-ports]` entries in `docs/plan/ARCHITECTURE.md`
§Processor model & scheduling, decided 2026-08-03. Supersedes
`data-plane-cast-not-contract.md`, which kept unilateral hints, an advisory connect
warning, and an inert wire tag — this decision removes all three.

## Trigger

Read this before adding any type, schema, or identity field to a port declaration;
before making `connect` inspect what either end carries; before reintroducing codegen,
a schema registry, or a `@org/package/Type` string anywhere in an authoring surface.

## Decision

A port declares name, description, and — on an input — delivery profile. Nothing else.
Type information belongs to the authoring language and never reaches the engine: a
Python port method's return annotation is documentation and type-checker input, and
`ctx.inputs.read(port, into=T)` is the reader's own strictness dial; a Rust read is
typed by the struct it deserializes into, with serde as the always-on validation. The
engine has no type layer at all — no declarations to compare, so no comparison, no
advisory warning, no wire tag, no schema registry, no codegen, no JTD.

A processor's identity is its class, named by its fully-qualified import path — one
string everywhere, including what the control plane prints. The `@org/package/Type@version`
grammar is deleted from every authoring surface.

## Rejected alternatives

- **Unilateral hints kept as advisory metadata** (the previous decision) — a declaration
  no code may act on is a field that reliably drifts from reality and misleads whoever
  reads it. Removing the comparison but keeping the hint bought nothing and cost a wire
  field, a stamping path, and a warn site.
- **`@org/package/Type@version` as port or processor identity** — module addressing that
  outlived its address space. With PyPI and cargo as the package systems and `rt.add`
  taking the class, the identity grammar names nothing the import system does not
  already name, and forces two representations of one processor. Empirically it is
  already broken: `schema=` on a port fails reads against the half-deleted registry.
- **Engine-side validation of the language's type declaration** — would require the
  engine to model Python's and Rust's type systems, and would recreate schema agreement
  under a new name.

## Consequences

- Nothing in the control plane can say what a port carries; `graph` and `tap` show
  name, description, delivery profile, direction. This is accepted cost, not oversight.
- The delivery-profile default is gone with the schema that carried `metadata.flow_class`,
  so every input port must declare one. The owner accepted this with reservation
  (2026-08-03: "we can do the recommended and change later, not crazy about this") —
  the alternatives were a flat default that silently drops audio samples, or inference
  from a type layer that no longer exists. Revisit if the explicit declaration proves
  noisy in real apps.
- `SchemaIdentWire` leaves the frame header; layout locks move with it.
- Identity and label separate cleanly: the import path is exact and machine-facing, the
  instance's display name is human-facing and may repeat. Engine-appended counters on
  duplicate labels are a convenience the owner accepted as revisitable (2026-08-03).
- The entry file must be imported, not executed as a script — otherwise a class defined
  in it identifies as `__main__:…`, which no helper process can import and which varies
  with the launch path.
- Cross-language and cross-node interop rest entirely on the bag being self-describing.
