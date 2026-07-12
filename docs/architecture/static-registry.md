# Static-file registry

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).

A registry's read side is just static files. StreamLib emits a plain on-disk
tree — a cargo sparse index + `.crate` tarballs, a PEP-503 pypi-simple tree, an
npm packument + `.tgz`, and the `.slpkg` generic store — that is **tokenless to
read** and **browsable as a plain HTTP directory index**. The same tree serves
identically whether it is a CI fixture, a local publish-and-read folder, or a
cloud object store. No Gitea daemon, no database, no token is required to
*serve* it.

## Per-ecosystem read transport

Each ecosystem is served over its native anonymous read transport. There is no
single transport because the ecosystems' clients differ:

| Ecosystem | Transport | Why |
|---|---|---|
| `.slpkg` generic | `file://` | The `streamlib-idents` registry client already reads the generic store over `file://` (`RegistryClient`); reused, not rebuilt. |
| pypi-simple | `file://` | uv/pip consume a PEP-503 `simple/` tree over `file://` natively (`UV_INDEX=file://…/pypi/simple`). |
| cargo sparse | dumb static HTTP mount (`sparse+http://…`) | The cargo sparse protocol is **HTTP-only by spec** — there is no `sparse+file://`. A static file server is not a registry daemon. |
| npm | same static HTTP mount | npm reads a packument (`GET /<name>`) + `.tgz`; `dist.tarball` points at the mount. |

The static HTTP mount is any dumb directory server — `python3 -m http.server`,
`nginx autoindex`, `caddy file_server`, an object-store/CDN origin. StreamLib
does **not** ship a server binary.

## Tree layout

```
<root>/
  cargo/
    config.json                         # {"dl":"<base>/cargo/crates/{crate}/{crate}-{version}.crate","api":"<base>/cargo"}
    <aa>/<bb>/<crate>                    # sparse index NDJSON (one line per version)
    crates/<crate>/<crate>-<version>.crate
  pypi/
    simple/index.html                   # PEP-503 root
    simple/streamlib/index.html         # per-project links (#sha256=…)
    packages/streamlib-<version>.tar.gz # sdist
  npm/
    @tatolab/streamlib-deno             # packument — a FILE at the package
                                        # URL path (npm GETs /npm/@scope%2fname;
                                        # a static server decodes to this path)
    tarballs/streamlib-deno-<version>.tgz  # dist.tarball points here
  slpkg/
    <pkg>/<version>/<pkg>.slpkg         # generic store (RegistryClient file:// layout)
    streamlib-release/<V>/manifest.json # the release manifest — completion marker
```

The cargo `config.json` `dl`/`api` and the npm `dist.tarball` are absolute URLs
(sparse + npm are HTTP), so they carry the **base URL** the tree is served at;
the `.crate`/`.tgz`/sdist/`.slpkg` bytes and the index files themselves are
relocatable.

## The vulkanalia fork is mandatory

The workspace declares `vulkanalia = { registry = "gitea" }`. With no committed
`Cargo.lock`, **no `cargo` command in the workspace resolves — not even
`cargo run -p xtask`** — until the fork (`vulkanalia`, `vulkanalia-sys`,
`vulkanalia-vma`) is fetchable. This is the standing CI red: jobs that build the
workspace died fetching the fork from `localhost:3300`.

Because building `xtask` itself requires the fork, the fork's cargo tree cannot
be produced by an `xtask` subcommand — it is emitted by the standalone shell
script [`scripts/gitea/emit-static-fork.sh`](../../scripts/gitea/emit-static-fork.sh),
which packages the fork from a standalone clone (the fork depends only on
crates.io and its own siblings, never the workspace or a registry daemon),
mirroring `publish-vulkanalia.sh` but writing a static file tree. CI serves it
with `python3 -m http.server` and points cargo at it via
`CARGO_REGISTRIES_GITEA_INDEX` (the `.github/actions/serve-static-fork`
composite action starts the server FIRST and passes `STATIC_FORK_URL`, so the
script skips its own throwaway server and resolves fork siblings through the
exact tree being populated — no port coupling). Same-registry index deps (fork
siblings, closure crates) omit the `registry` key — detected data-driven from
the packaged manifest's `registry-index` key; crates.io deps name
`https://github.com/rust-lang/crates.io-index`.

The **workspace release closure** rides the same tree via
`cargo xtask static-registry emit --cargo-closure`: each closure crate is
`cargo package`d in topo order against an ephemeral static server on the
staging tree itself (each crate resolves its already-emitted siblings + the
fork from the growing staging index; `cargo package` validates registry deps
at package time). Versions always follow the crates' actual manifests — a
`--dev` emit expects the workspace manifests already bumped (the publish
scripts' bump/restore convention).

## Atomic release — the staged swap

A `file://` consumer must never observe a half-written tree. `emit_static_registry`
builds the whole tree into a **staging sibling** of the served path and writes
the [`ReleaseManifest`](../../libs/streamlib-idents/src/release.rs) LAST, then
flips staging into the served path in a single operation
(`static_registry::publish_staged_tree`): a plain atomic `rename` when the
served path is absent, and a gapless `renameat2(RENAME_EXCHANGE)` swap when
replacing an existing tree (Linux). During the (long) staging build the served
tree is a separate directory and is never touched, so a concurrent reader always
sees the previous *complete* release; the flip is the only mutation of the
served path. This closes the mid-publish window where a consumer could cargo-
resolve a higher partial version before its release manifest landed.

## Emitting a tree

```
cargo xtask static-registry emit --out <dir> [--dev N] \
    [--base-url http://127.0.0.1:PORT] \
    [--cargo-closure] [--no-cargo-fork] [--no-pypi] [--no-npm] [--no-slpkg]
```

- The vulkanalia fork cargo tree is always emitted (unless `--no-cargo-fork`).
- `--cargo-closure` additionally packages every workspace release-closure crate
  into the cargo tree (heavy).
- pypi (uv sdist) and npm (deno pack) reuse the SDK build toolchains; `.slpkg`
  packages are assembled via `streamlib pack` semantics and the release manifest
  is written last.

## Consuming a tree

Serve the tree and export the four env channels — one helper does both:

```
scripts/gitea/serve-static-registry.sh <dir> [--port 8799]
```

It starts `python3 -m http.server` on the tree and prints the env a consumer
sets (both the in-process codegen channel and the ecosystem clients — see
[`polyglot-venv-gitea-registry-env`](../learnings/polyglot-venv-gitea-registry-env.md)
for why *both* `STREAMLIB_REGISTRY_*` and `UV_INDEX` are needed):

```bash
# .slpkg + in-venv codegen (file://)
export STREAMLIB_REGISTRY_URL="file://<dir>/slpkg"
# pypi (file://)
export UV_INDEX="file://<dir>/pypi/simple"
# cargo (static HTTP mount)
export CARGO_REGISTRIES_GITEA_INDEX="sparse+http://127.0.0.1:8799/cargo/"
# npm (static HTTP mount) — .npmrc:
#   @tatolab:registry=http://127.0.0.1:8799/npm/
```

## Reference

- **Renderers + atomic swap**: `libs/streamlib-pack/src/static_registry.rs`.
- **Fork bootstrap**: `scripts/gitea/emit-static-fork.sh`,
  `scripts/gitea/render_cargo_index_line.py`.
- **Generator CLI**: `cargo xtask static-registry emit`.
- **`.slpkg` `file://` transport**: `libs/streamlib-idents/src/registry.rs`.
- **CI**: `.github/actions/serve-static-fork`, `.github/workflows/static-registry.yml`.
