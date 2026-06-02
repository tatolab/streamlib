# `plugin/` — the engine-free plugin zone

Every crate in this folder is **engine-free**: it must not depend on
`streamlib-engine`, directly or transitively. These are the only crates
safe to compile into a plugin `.slpkg` cdylib.

This is the authoring + shared-library surface for out-of-`libs` plugins
(packages). A plugin processor depends on crates **here** — the
plugin-SDK, the plugin ABI, shared helper libs — **never** on `libs/`.

**The dependency arrow points `libs/` → `plugin/`, never back.** A
`plugin/` crate that gains a `libs/` (engine-bound) dependency is a bug;
`cargo xtask check-boundaries` bans it.

Why this zone exists: linking the full engine into a plugin cdylib ships
a *second copy* of `streamlib-engine` into the host process. Two engine
copies fight over duplicated process-global state (Vulkan dispatch
table, signal/panic hooks, `PUBSUB`, the escalate gate) and corrupt the
NVIDIA driver during concurrent GPU setup. Keeping this zone engine-free
*by folder construction* makes that mistake impossible to make by
accident — you can't write `use streamlib::sdk::…` (which pulls the
engine) from a `plugin/` crate, because the engine isn't reachable here.
