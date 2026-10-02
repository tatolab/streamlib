# A built-in node's type

Rationale for the `[built-in-node-type]` entry in `docs/plan/ARCHITECTURE.md` §Processor model &
scheduling, decided 2026-10-02 in the one-runtime-per-machine pivot.

## Trigger

Read this before deciding how a node class is named in a graph, before writing a built-in's type
string by hand, before renaming, moving or splitting a crate or module that holds built-ins, and
before adding anything that maps one type name to another.

## Decision

A built-in's type is the import path of its class in `tatolab.stream` —
`tatolab.stream:CameraSource` — and the runtime registers the built-in under that string. Every
node's type is the import path of the class a stream names: the runtime resolves its own
built-ins by that string, and imports any other in the stream's interpreter.

## Why the Rust path stopped being private

Before the package split, the library that wrote a type and the engine that read it were one
wheel at one version, so a built-in's Rust module path was an internal detail. After the split
they are two products, installed and updated apart: each project's venv holds its own
`tatolab-stream`, at the version it locked, and the machine's one runtime is updated by the
installer. The stream library prints each built-in's type into its class, so every compile by an
older library emits the types that library was published with — fresh runs, not only recorded
graphs. Since a stream never names a runtime version, that type is the contract between the two
products, and a runtime that moved its built-ins' Rust modules would refuse every stream an older
library compiled, though the stream's source is unchanged and correct.

A node the project writes is the other case: the project writes its type and the project's own
interpreter imports it, so both ends move together and its location is the right name. Long-lived
graph tools name what they ship the same way — GStreamer pipelines by element name, ComfyUI
workflows by node class name — while their code moves underneath.

## Rejected alternatives

- **The Rust module path, frozen.** Keeps today's strings by forbidding the built-ins' crate and
  modules from ever moving, so internal layout becomes a public contract, and graphs carry the
  retired `streamlib` name for good.
- **Renaming the built-ins' crate to its final name first, then freezing it.** The same freeze,
  one rename later.
- **A table from public names to Rust paths.** Two names for one thing and a table to keep in
  step; owner, 2026-10-02: no translation.
- **A short registered name outside the import-path form** (`CameraSource` alone). Stable, but a
  second grammar beside `module:qualname`, where the graph names every node by its class's import
  path.
- **Types follow the code, and a rename replaces the golden graphs while nobody uses the
  project.** Ships a contract known to be wrong and hands the same break to the first user.

## Consequences

- Rust mints a built-in's type at the authoring seam from its class name under `tatolab.stream`,
  never from a reflection API; the Rust struct's name is the public class name.
- The classes `tatolab.stream` carries for built-ins hold the string the runtime registers, byte
  for byte, and a test holds the two equal.
- Renaming a built-in's public class breaks a stream's source and its graph together; nothing else
  changes a type. Renaming the Rust crates changes no graph.
- The golden graphs carry these types from the first one checked in.
