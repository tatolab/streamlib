# Vendored vulkanalia fork — `vendor/tatolab-vulkanalia*`

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

## What this is

The tatolab fork of [vulkanalia](https://github.com/KyleMayes/vulkanalia)
is vendored into this repo as three ordinary workspace members:

| Directory | Package name | Version | Upstream crate |
|---|---|---|---|
| `vendor/tatolab-vulkanalia` | `tatolab-vulkanalia` | 0.35.0 | `vulkanalia` |
| `vendor/tatolab-vulkanalia-sys` | `tatolab-vulkanalia-sys` | 0.35.0 | `vulkanalia-sys` |
| `vendor/tatolab-vulkanalia-vma` | `tatolab-vulkanalia-vma` | 0.9.0 | `vulkanalia-vma` (`ext/vma` in the fork) |

Each keeps its upstream `[lib] name` (`vulkanalia`, `vulkanalia_sys`,
`vulkanalia_vma`), and `[workspace.dependencies]` uses `package =`
renames, so every consumer keeps writing `use vulkanalia::…` /
`vulkanalia = { workspace = true }` unchanged. A cold clone builds with
zero registry configuration — the crates resolve by `path` like any
other workspace member.

## Drift guard — no in-place edits, no fmt sweeps

`cargo xtask check-vendored-vulkanalia` pins one deterministic content
hash per vendored crate dir (recorded in
`xtask/src/check_vendored_vulkanalia.rs`, run by the check-boundaries CI
workflow and by `cargo test -p xtask`). Any byte change — an edit, a
reformat, an added/removed/renamed file — fails with a message naming
the drifted dir. This is the enforcement behind the verbatim-copy
contract; prose alone cannot stop a routine workspace `cargo fmt --all`
sweep from rewriting vendored sources (`cargo fmt --check` already
disagrees with the vendored formatting, and no stable rustfmt exclusion
mechanism exists — `rustfmt.toml`'s `ignore` is nightly-only). There is
no `cargo fmt` CI gate today; if one is ever added it must skip the
three vendored dirs explicitly (e.g. run `cargo fmt -p <crate>` on
non-vendored members), with this hash guard as the backstop. Workspace
fmt sweeps must exclude `vendor/tatolab-vulkanalia*`.

## License

The vendored crates are **Apache-2.0** (upstream vulkanalia's license;
`LICENSE.txt` is copied into each directory). This is a deliberate
exception to the repo-wide BUSL-1.1 new-file header rule: **do not add
BUSL headers to any file under `vendor/tatolab-vulkanalia*`**, and do not
reformat or "improve" the vendored sources. The embedded third-party
headers keep their own notices (VulkanMemoryAllocator's MIT notice is
embedded at the top of `vk_mem_alloc.h`; the Vulkan headers are
Apache-2.0).

## Why the rename

The fork publishes patches upstream does not have, but historically
shared upstream's crate names **and** version numbers — selectable only
by a custom-registry annotation. That shadow made every cold build
depend on registry bootstrap machinery. Renaming to `tatolab-vulkanalia*`
at vendor time means the crates can never collide with a transitive
crates.io `vulkanalia` in any dep graph, and the `package =` rename
keeps the source-level API identical.

## Fork patches carried

Relative to crates.io upstream (still 0.35.0 / 0.9.0), the fork rev
vendored here carries:

- VMA 3.3.0 + Vulkan-Headers 1.4.309 (submodule bumps in `vulkanalia-vma`'s
  `vendor/` tree).
- `ShaderModuleCreateInfoBuilder::code_size` builder restoration
  (`vulkanalia/tests/builders.rs` locks it — the tests are vendored too).
- `vulkanalia-vma` standalone publishability (its `vulkanalia` dep
  resolves by version, no monorepo `path`).
- `video.rs` bindgen revert (bindgen 0.68.1-flavored output).

## Local patches on top of the fork rev

The vendored copy is byte-identical to the fork rev except:

1. **Manifest edits** — package renames, `[lib]` name pins,
   `readme = "README.md"` (the fork pointed at a repo-root README),
   sibling deps rewritten to
   `{ package = "tatolab-…", path = "../tatolab-…", version }` (resolved by
   `path` in-workspace; the custom `registry = "tatolab"` key was removed with
   the cargo registry in #1322),
   and `[lib] doctest = false` on `tatolab-vulkanalia` (its `bytecode.rs`
   doctests `include_bytes!` shader fixtures from the fork repo's
   `tutorial/` + `examples/` dirs, which the trim rule drops; the unit
   tests in `tests/builders.rs` are the behavior lock instead).
2. **`src/vk/builders.rs` (`tatolab-vulkanalia`)** — one token,
   `std::mem::size_of` → `core::mem::size_of` in
   `ShaderModuleCreateInfoBuilder::code`. The fork's code_size fix used
   `std::` in a `#![no_std]`-capable crate, which fails to compile
   whenever the crate is built with `default-features = false` (as
   `tatolab-vulkanalia-vma` does). Whole-workspace feature unification
   masked it; standalone / packaged builds do not. Fix this in the fork
   repo on the next rebase so a re-copy doesn't reintroduce it.

## Provenance

- Fork repo: `github.com/tatolab/vulkanalia` (branch of
  `KyleMayes/vulkanalia`).
- Vendored rev: `982d32d` ("Revert video.rs to base branch (bindgen
  0.68.1)").
- Submodules at that rev: `Vulkan-Headers` @ `952f776` (v1.4.309),
  `VulkanMemoryAllocator` @ `1d8f600`.

The fork repo stays alive as the **rebase workbench only** — it is not
in the build path. Rebasing onto a new upstream vulkanalia happens
there (where upstream git history and the generator live), then the
result is re-copied here per the recipe below.

## The vendor trim rule

Only what the build needs is vendored; upstream's generator, examples,
tutorial, and layer directories are not copied (the Rust bindings are
checked in; bindgen runs only behind `tatolab-vulkanalia-vma`'s optional
`bind` feature). For `tatolab-vulkanalia-vma`'s C sources the rule is
mechanical:

- `vendor/VulkanMemoryAllocator/include/` — copied whole (one file,
  `vk_mem_alloc.h`).
- `vendor/Vulkan-Headers/include/` — **C-only subset**:
  `vulkan/*.h` + `vk_video/*.h`. The C++ headers (`*.hpp`, `*.cppm`,
  ~18 MB) are dropped; `wrapper.cpp` includes only `vk_mem_alloc.h`,
  which includes `<vulkan/vulkan.h>` — the C surface.

## Update recipe

1. In the fork repo (`github.com/tatolab/vulkanalia`), rebase the patch
   set onto the new upstream tag; update submodules; run the fork's own
   tests. Apply the `core::mem` fix from the local-patches list above if
   not yet upstreamed into the fork.
2. Re-copy into this repo:
   - `vulkanalia/{Cargo.toml,src,tests}` → `vendor/tatolab-vulkanalia/`
   - `vulkanalia-sys/{Cargo.toml,src}` → `vendor/tatolab-vulkanalia-sys/`
   - `ext/vma/{Cargo.toml,build.rs,wrapper.cpp,src,README.md,CHANGELOG.md}`
     → `vendor/tatolab-vulkanalia-vma/`, plus the trimmed `vendor/` tree
     per the rule above; repo-root `README.md` + `LICENSE.txt` into all
     three.
3. Re-apply the manifest edits (package renames, `[lib]` names, sibling
   dep rewrites — diff against the previous vendored manifests).
4. Record the new rev + submodule SHAs in the Provenance section above.
5. Re-capture the drift-guard hashes: run
   `cargo xtask check-vendored-vulkanalia` — it fails printing the new
   per-dir hashes — and update `VENDORED_TREES` in
   `xtask/src/check_vendored_vulkanalia.rs` with them **in the same
   commit** as the re-vendor.
6. `cargo check -p tatolab-vulkanalia-vma` (catches no_std breakage
   standalone builds hit that workspace builds mask), then the workspace
   test baseline per `docs/testing-hardware.md`.
