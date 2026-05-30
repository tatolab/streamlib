# Gitea registry tooling

Scripts for the unified self-hosted **Gitea** registry that distributes every
StreamLib-authored / -customized artifact under the **`tatolab`** org. The
committed scripts are **generic, configure-by-env** so the same tooling drives
a local dev container and a hosted backend. Architecture:
[`docs/architecture/gitea-registry-distribution.md`](../../docs/architecture/gitea-registry-distribution.md).

| Script | What it does | When |
|---|---|---|
| `provision-registry.sh` | Ensure the admin owner + `GITEA_ORG` exist; verify cargo/pypi/npm/generic are reachable. | One-time / idempotent. |
| `smoke-test-registry.sh` | Publish→resolve→remove a throwaway crate + a generic round-trip. | Verify a live registry. |
| `publish-vulkanalia.sh` | Publish the `tatolab/vulkanalia` fork (`-sys`, `vulkanalia`, `-vma`) to the cargo registry. | One-time bootstrap (fork rarely changes). |
| `publish-crates.sh [--dev N]` | Publish the `streamlib` SDK crate closure by version, in topo order. | The recurring dev-loop publish. |
| `migrate-internal-deps.py` | Idempotent tomlkit sweep that put internal deps in the `{ path, version, registry }` form. | One-shot migration (kept for re-derivation / drift check). |

The Python helpers need **tomlkit** (`pip install tomlkit`, or run with
`PYTHON=/path/to/venv/bin/python`).

## Config & secrets

- The repo's [`.cargo/config.toml`](../../.cargo/config.toml) declares
  `[registries.gitea]` with the **sparse** index URL. Read is anonymous; only
  publishing needs a token. Override the URL for a hosted backend with
  `CARGO_REGISTRIES_GITEA_INDEX`.
- **Secrets never live in committed scripts.** Access topology (admin user,
  port, token) goes in a gitignored `scripts/gitea/*.local.sh` wrapper that
  exports env and `exec`s the generic script — see
  `provision-registry.local.sh` for the pattern.

### Minting a publish token

Any token with `write:package` scope works. For the local dev container:

```bash
docker exec -u git streamlib-registry \
  gitea admin user generate-access-token \
  --username tatolab-admin --scopes write:package --token-name publish
```

cargo requires the token stored as **`Bearer <token>`** (a bare `cargo login`
value → 401). Provide it via env:

```bash
export CARGO_REGISTRIES_GITEA_TOKEN="Bearer <token>"
```

## Publishing

```bash
# one-time: publish the vulkanalia fork first (needs the fork checked out at
# VULKANALIA_DIR, default ~/Repositories/tatolab/vulkanalia)
CARGO_REGISTRIES_GITEA_TOKEN="Bearer <token>" scripts/gitea/publish-vulkanalia.sh

# then publish the streamlib SDK closure at the base [workspace.package].version
CARGO_REGISTRIES_GITEA_TOKEN="Bearer <token>" scripts/gitea/publish-crates.sh

# dev loop: publish 0.4.x-dev.N so a consumer can bump to it without a path dep
CARGO_REGISTRIES_GITEA_TOKEN="Bearer <token>" scripts/gitea/publish-crates.sh --dev 3
```

`--no-verify` publishes source without compiling (the consumer verifies by
building); both scripts treat an already-published version as success, so they
are safe to re-run. The closure + topo order is derived live from
`cargo metadata`. `publish-crates.sh` strips the dev `path:` patch from any
bundled `streamlib.yaml` (today: `streamlib-engine` →`@tatolab/escalate`) and
restores the tree afterward, so the published manifest is path-free and the
consumer resolves schema deps from the registry.

### Schema packages (`.slpkg`) for the full-engine codegen build

A consumer compiling `streamlib-engine` runs its `build.rs` schema codegen,
which resolves `@tatolab/escalate` from the **generic** registry. Publish the
schema package(s) as source `.slpkg`s:

```bash
streamlib pack packages/escalate -o /tmp/escalate.slpkg
curl -X PUT -H "Authorization: token <token>" --upload-file /tmp/escalate.slpkg \
  http://localhost:3300/api/packages/tatolab/generic/escalate/1.0.0/escalate.slpkg
```

Note the two distinct Gitea auth schemes: the **generic** registry (and the
package-management API) takes `Authorization: token <token>`, whereas **cargo
publish** requires the token stored as `Bearer <token>` (see above). Don't
conflate them. Gitea's generic upload also needs a raw body (`--upload-file`,
not `--data`).

(Generalizing schema/processor `.slpkg` publishing across all `packages/*` is
the source-only-`.slpkg` work tracked separately.)

## Consuming from a separate repo

```toml
# consumer Cargo.toml
[dependencies]
streamlib = { version = "0.4.30", registry = "gitea" }
```

```toml
# consumer .cargo/config.toml
[registries.gitea]
index = "sparse+http://localhost:3300/api/packages/tatolab/cargo/"
```

To compile the engine's schema codegen, the build needs the generic-registry
URL + a read token in the environment:

```bash
STREAMLIB_REGISTRY_URL=http://localhost:3300 \
STREAMLIB_REGISTRY_TOKEN=<token> \
  cargo build
```
