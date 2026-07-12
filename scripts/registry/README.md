# Static registry tooling

Scripts that build and serve the streamlib **static file-tree registry** — a
plain on-disk tree (cargo sparse index + `.crate` tarballs, pypi-simple, npm
packument + `.tgz`, the `.slpkg` generic store, and the catalog) that is
tokenless to read and served by any dumb HTTP mount. There is no registry
daemon. See
[`docs/architecture/static-registry.md`](../../docs/architecture/static-registry.md).

## Scripts

| Script | What it does |
|---|---|
| `emit-static-fork.sh` | Package the `tatolab/vulkanalia` fork (`vulkanalia`, `-sys`, `-vma`) into a cargo sparse subtree. The daemon-free bootstrap: the workspace declares `vulkanalia = { registry = "tatolab" }`, so cargo cannot resolve until the fork is fetchable. Emitted from a standalone clone (the fork depends only on crates.io + itself). |
| `render_cargo_index_line.py` | Render one cargo sparse-index NDJSON line from a `.crate`'s bundled manifest — the single source of truth for the index-line shape. |
| `cargo-idx-path.sh` | Compute a crate's RFC 2141 sparse-index shard path (`serde → se/rd/serde`), sourced by `emit-static-fork.sh`. |
| `serve-static-registry.sh` | Serve an emitted tree with `python3 -m http.server` and print the consumer configuration. The manual equivalent of `streamlib registry use <dir>`. |
| `migrate-internal-deps.py` | Rewrite workspace cross-crate deps to the `{ path, version, registry = "tatolab" }` form. |

## Emitting a full tree

The whole tree (fork + workspace closure + pypi + npm + `.slpkg` + catalog) is
produced by the workspace tool, not these scripts:

```bash
cargo xtask static-registry emit --out <dir> [--dev N] [--cargo-closure]
```

`emit-static-fork.sh` is the one piece that must run standalone (building
`xtask` itself needs the fork), so the emitter shells out to it for the fork
subtree.

## Consuming a tree

```bash
scripts/registry/serve-static-registry.sh <dir> [--port 8799]
```

prints the consumer env (`STREAMLIB_REGISTRY_URL=file://<dir>`,
`UV_INDEX=file://<dir>/pypi/simple`, the cargo `[source]` replacement, and the
npm `.npmrc` line). For the ergonomic path use `streamlib registry use <dir>`,
which writes the cargo/npm config into the consumer and auto-serves npm on
localhost. cargo/npm read over a static HTTP mount (sparse + npm are HTTP-only);
pypi + `.slpkg` read straight off `file://`.
