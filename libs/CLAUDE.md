# `libs/` — the engine-internal zone

Crates here are compiled **with** `streamlib-engine` and may use engine
internals freely. This is the host side: the engine itself, the
`streamlib` SDK facade (Tier-1/2/3 surface for in-process apps that link
the engine), the build orchestrator, surface-client, the CLI.

**A `libs/` crate must NEVER be a dependency of a plugin `.slpkg`
cdylib.** That compiles the engine into the plugin — a second engine in
the process, which corrupts the GPU driver (see `../plugin/CLAUDE.md`).
Plugins depend on the engine-free crates in `../plugin/` instead.

Dependency arrow: `libs/` **may** depend on `plugin/` (e.g. the engine
re-exports the shared `streamlib-error` type, or imports a plugin-zone
shared helper). `plugin/` may **not** depend on `libs/`. `cargo xtask
check-boundaries` enforces both directions.

Engine-free crates that currently still live here (`streamlib-plugin-abi`,
`streamlib-processor-schema`, `streamlib-consumer-rhi`) belong in
`../plugin/` and move there in a follow-up; their being engine-free is
what makes that move mechanical.
