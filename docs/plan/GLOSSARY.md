# Glossary

The shared language. Terms only — zero implementation detail. Maintained by the
`glossary` skill inside plan-editing sessions; project-specific terms only.

**Host**: the engine process that loads plugins. _Avoid_: "runtime process", "main app".

**Plugin**: a package's compiled cdylib loaded by the host across the plugin ABI.
_Avoid_: DSO, shared object, FFI module.

**Plugin ABI**: the `#[repr(C)]` boundary between host and plugin. _Avoid_: FFI, COM.

**Package**: the distributable unit (source tree + `streamlib.yaml`) resolved by version
from a package source. _Avoid_: "plugin" (that is the compiled artifact).

**Package source**: the store packages resolve from at build time — never a sibling
directory.

**Link**: the sole local-development path for consuming a package from a folder.
`add`/`install` take finalized artifacts only.

**App**: a consumer of packages — an entry file (`app.py` defining `setup(rt)`) plus
`streamlib.lock` and `streamlib_modules/`; carries no manifest. Promotes to a package by
adding the identity label. _Avoid_: "project", "consumer app" (redundant).

**Bag**: the self-describing msgpack named map a link carries — the schema-free view of
a payload; consumers cast it to a type at read time. _Avoid_: "message", "envelope".

**Control plane**: the HTTP/WebSocket/MCP surface a runtime hosts for inspection and
mutation; the CLI and client SDKs are its clients. _Avoid_: "API server" as the concept
(that is the component hosting it).

**Node**: a live runtime reachable over its control plane; discovered via the per-user
on-disk registry. _Avoid_: "instance", "server".

**Processor**: the unit of pipeline computation, declared with `#[processor]` and wired
by ports.

**The plan**: `docs/plan/ARCHITECTURE.md` plus `docs/plan/diagrams/` — the single source
of architectural decisions.

**Change**: a typed delta proposal against the plan (`docs/plan/changes/`), marked
ADDED / MODIFIED / REMOVED.
