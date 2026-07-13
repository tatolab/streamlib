# Static registry tooling

Scripts that build and serve the streamlib **static file-tree registry** — a
plain on-disk tree (cargo sparse index + `.crate` tarballs, pypi-simple, npm
packument + `.tgz`, the `.slpkg` generic store, and the catalog) that is
tokenless to read and served by any dumb HTTP mount. There is no registry
daemon. See
[`docs/architecture/static-registry.md`](../../docs/architecture/static-registry.md).

## Scripts

(Rows for `emit-static-fork.sh` and `normalize_fork_crate.py` removed — the
vulkanalia fork is vendored at `libs/tatolab-vulkanalia*` and resolves by path,
so the fork-bootstrap emit/normalize machinery was deleted; see
`docs/architecture/vendored-vulkanalia.md`.)

| Script | What it does |
|---|---|
| `emit-cargo-local-registry.sh` | Reshape an emitted cargo **sparse** subtree into a cargo **local-registry** (`index/<shard>` + flat `.crate`s, no `config.json`) so cargo resolves the `tatolab` registry over a `file://` `[source]` replacement with **no HTTP server**. The serverless resolve CI uses (check-pack-load's out-of-tree build) and the offline path an external consumer rides. |
| `render_cargo_index_line.py` | Render one cargo sparse-index NDJSON line from a `.crate`'s bundled manifest — the single source of truth for the index-line shape. |
| `cargo-idx-path.sh` | Compute a crate's RFC 2141 sparse-index shard path (`serde → se/rd/serde`); standalone helper, exercised against the Rust twin by `streamlib-pack`'s golden tests. |
| `serve-static-registry.sh` | Serve an emitted tree with `python3 -m http.server` and print the consumer configuration (the manual configure-a-consumer path; a `streamlib registry use <dir>` verb is planned). |
| `migrate-internal-deps.py` | Rewrite workspace cross-crate deps to the `{ path, version, registry = "tatolab" }` form. |

## Emitting a full tree

The whole tree (workspace closure — vendored `tatolab-vulkanalia*` included —
+ pypi + npm + `.slpkg` + catalog) is produced by the workspace tool, not
these scripts:

```bash
cargo xtask static-registry emit --out <dir> [--dev N] [--cargo-closure]
```

## Consuming a tree

```bash
scripts/registry/serve-static-registry.sh <dir> [--port 8799]
```

prints the consumer env (`STREAMLIB_REGISTRY_URL=file://<dir>`,
`UV_INDEX=file://<dir>/pypi/simple`, the cargo `[source]` replacement, and the
npm `.npmrc` line). A `streamlib registry use <dir>` verb that writes that
cargo/npm config into the consumer and auto-serves npm on localhost is planned.
npm reads over a static HTTP mount (HTTP-only); pypi + `.slpkg` read straight
off `file://`. cargo has two shapes: a `sparse+http` `[source]` replacement over
the served mount, **or** a serverless `local-registry` `file://` replacement
built by reshaping the sparse tree with `emit-cargo-local-registry.sh` (no
server — what CI + `--locked --offline` resolves use). Both keep the canonical
source id in `Cargo.lock`; see
[`docs/architecture/static-registry.md`](../../docs/architecture/static-registry.md)
→ "Consuming a tree".
