# Zero-ceremony authoring

> Current known state of the authoring surface. Subject to staleness or
> drift — verify against the code before relying on any claim. Not
> authoritative, not enforcement.

## What this document describes

How a processor is authored today: identity and ports declared **in
code**, configuration carried as a dynamic [`Bag`], a graph wired
directly through `App` sugar, and packaging reserved for the moment a
processor is *shared*. The through-line is that nothing on the authoring
path requires a manifest, a config schema, a build step, or codegen. A
bare source module is a working local processor; ceremony is opt-in and
scoped to the consumer that actually shares wire vocabulary.

## The processor is the code

A processor's identity, execution mode, scheduling, and ports are
declared on the type itself. No sibling `streamlib.yaml` is read to
learn what a processor is or what it declares — the decorator (Python /
Deno) and the `#[processor]` proc-macro (Rust) are the truth-source.

- **Rust** — `#[streamlib::sdk::processor("@org/package/Type", execution
  = manual)]` on a struct. The macro synthesizes identity and the
  runtime registers the type; a `#[processor]` type in a plain crate,
  with no package and no build, is a complete processor.
- **Python** — `@processor("@org/package/Type", execution="manual")` on
  a class. Identity, execution, and scheduling come from the decorator
  arguments; nothing is read from disk at decoration time.
- **Deno** — `@processor("@org/package/Type", { execution: "manual" })`
  on a class, same contract.

The identity string is **version-free** (`@org/package/Type`, no
`@version`): a schema reference is an identity the runtime binds
version-blind, and the concrete version is derived at package-build
time, never hand-authored. Omitting the identity entirely synthesizes
`@app/local/<TypeName>` — a bare module with no manifest defines a
working local processor.

The package's processor set is derived by **importing the modules and
enumerating what registered**, never by reading a hand-authored
`processors:` list out of a manifest. In Rust the equivalent is a `syn`
source-scan that reads the AST without running it; in Python and Deno
extraction *is* import.

## Config is a `Bag`

A processor's configuration does not require a schema. [`Bag`] is a
dynamic, self-describing msgpack named-map payload: a processor declares
`type Config = Bag` (Rust) — or accepts a plain dict / object
(Python / Deno) — and reads the fields it needs at runtime with named,
typed accessors. A missing key and a wrong-typed key are distinct named
errors, never a panic and never an untyped failure.

`Bag` is the schema-free counterpart to the typed `read::<T>` /
`write::<T>` paths. It encodes byte-for-byte as the same msgpack named
map a generated config struct would, so a `Bag`-typed config crosses the
plugin ABI exactly like a codegen struct, and a processor can migrate
between the two without a wire change. There is no mandatory config
schema on the authoring path.

## Wiring: `App` sugar

`App` is thin authoring sugar over the runtime `Runner` — it holds one
`Runner`, adds no runtime state of its own, and forwards every call. For
anything the sugar omits, `App::runner` is the escape hatch back to the
full surface.

- **`App::add(processor_ref, config)`** — add a processor by version-free
  type reference, configured from any serializable value (a generated
  config struct, a plain struct, or a `Bag`). A reference to a
  not-yet-built package materializes from source on demand.
- **`App::add_local::<P>(config)`** — register a `#[processor]` host type
  `P` live, with **no package on disk**, and instantiate it in one call.
  This is the zero-ceremony hello-world path: a `#[processor]` type in
  the same crate becomes a connectable node with no manifest, no build,
  and no staging.
- **`App::connect((&from, "out"), (&to, "in"))`** — connect an output
  endpoint to an input endpoint by the `ProcessorUniqueId` an
  `add`/`add_local` call returned and the source-declared port name. A
  nonexistent port surfaces the runtime's typed
  `ProcessorPortNotFound` unchanged.
- **`App::run()`** — start the graph and block until a shutdown signal.

## Packaging is only for sharing

Nothing above needs a package. A `streamlib.yaml` manifest, a staged
package cache slot, and a published `.slpkg` exist to **share** a
processor across projects or machines — they are distribution mechanics,
not an authoring requirement. A processor you only run locally never
acquires a manifest. When you do package for sharing, the staged artifact
is a faithful mirror of the authored source tree (see
[`package-staging-layout.md`]); packaging adds distribution metadata, it
does not change how the processor is authored.

## The two-door descriptor model

Schema authoring collapses to two doors, and the second is optional:

1. **Self-describing wire** — send / receive with **zero type**. A
   processor emits and consumes msgpack named maps (`Bag`) directly; the
   wire carries its own field names, so no schema, no generated type, and
   no codegen is needed to move data between processors.
2. **An optional by-ID JTD descriptor** — for validation, the visual
   builder, and opt-in typed views. It is referenced by identity and
   **consumed as data, with no codegen**. A processor that wants
   validation or a builder entry points at a descriptor by ID; the
   descriptor is loaded and interpreted, never compiled in.
3. **`streamlib generate` typed views are opt-in sugar only** — running
   codegen to import a generated struct/dataclass/interface is a
   convenience for authors who want static field access, never a
   requirement for correctness or for moving data.

Ceremony-death is scoped to the **consumer**: the `schemas:` closure
map, `build.rs`, the `_generated_` tree, and `streamlib-codegen.lock` all
belong to the opt-in typed-view path and disappear from a processor that
stays on the self-describing wire. A **shared vocabulary type** — a wire
contract two independently-built processors must agree on — still carries
one small authored contract-only JTD file: the by-ID descriptor above.
That single authored contract is the floor; there is no ceremony below it.

## Standing guidance: the separate-build GPU hazard at the share boundary

A **shared** processor is built separately from the host that loads it (a
prebuilt cdylib per packing host). GPU code inside such a separately-built
processor must route through the cdylib-safe `FullAccess` primitives —
`ctx.gpu_full_access().…` — never a hand-rolled RHI call against a device
handle transited across the plugin ABI. The raw-device transit slots that
once made this a live hazard have been removed, so this is **standing
guidance**, not an open landmine: the safe primitives are the only door,
and there is no raw-Arc device to hand-roll against. Refcount and device
lifetime accounting run in host-compiled code via the clone/drop slots;
the plugin never touches the host's device directly. See
[`cdylib-reachability.md`] and [`plugin-abi.md`].

## Reference

- **`App` sugar** — `App::add` / `add_local` / `connect` / `run` in
  `sdk/streamlib-sdk/src/sdk/app.rs`, exercised in
  `sdk/streamlib-sdk/tests/app_sugar_test.rs`.
- **`Bag`** — `sdk/streamlib-plugin-sdk/src/bag.rs`.
- **`#[processor]` extraction** — `sdk/streamlib-macros`,
  `sdk/streamlib-processor-extract`; the Python decorator in
  `sdk/streamlib-python/python/streamlib/decorators.py`; the Deno
  decorator in `sdk/streamlib-deno/decorators.ts`.
- **Related**:
  - [`schema-identity-and-packaging.md`] — schema identity grammar and
    the codegen path the opt-in typed views ride.
  - [`package-staging-layout.md`] — how a shared package is laid out.
  - [`plugin-abi.md`] / [`cdylib-reachability.md`] — the plugin ABI and
    the cdylib-safe `FullAccess` primitives the share-boundary guidance
    rests on.

[`Bag`]: ../../sdk/streamlib-plugin-sdk/src/bag.rs
[`package-staging-layout.md`]: package-staging-layout.md
[`schema-identity-and-packaging.md`]: schema-identity-and-packaging.md
[`plugin-abi.md`]: plugin-abi.md
[`cdylib-reachability.md`]: cdylib-reachability.md
