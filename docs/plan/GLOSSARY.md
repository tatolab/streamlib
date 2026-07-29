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

**Processor**: the unit of pipeline computation, declared with `#[processor]` and wired
by ports.

**The plan**: `docs/plan/ARCHITECTURE.md` plus `docs/plan/diagrams/` — the single source
of architectural decisions.

**Change**: a typed delta proposal against the plan (`docs/plan/changes/`), marked
ADDED / MODIFIED / REMOVED.
