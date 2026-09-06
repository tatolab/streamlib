# Vendored moq-transport — `packages/streamlib-moq/vendor/moq-transport`

> Current shipped state only, per
> [`.claude/rules/docs-policy.md`](../../.claude/rules/docs-policy.md).

## What this is

The MoQ extension wheel carries its own copy of
[moq-transport](https://github.com/cloudflare/moq-rs) 0.16.2 — the draft-16
line, which is what Cloudflare's relays speak — as a path dependency of the
wheel's standalone workspace:

| Directory | Package name | Version | Upstream crate |
|---|---|---|---|
| `packages/streamlib-moq/vendor/moq-transport` | `moq-transport` | 0.16.2 | `moq-transport` (crates.io) |

The package keeps its upstream name and `[lib] name`, so every consumer keeps
writing `use moq_transport::…` unchanged. No rename is needed, unlike the
vulkanalia fork: nothing else in the wheel's dependency graph resolves a
crates.io `moq-transport` (the `moq-catalog` test oracle depends on `serde`
alone), so the path crate cannot collide with a registry copy. Because the
tree sits inside the wheel's workspace root it is an automatic workspace
member, which is what puts it under the wheel's `cargo fmt --all --check`.

It lives under the wheel rather than under the repo-root `vendor/` because
maturin's sdist root is the wheel directory: a path dependency outside it
would not reach the manylinux release build.

## Why vendored

The publisher needs behaviour the crate does not have — abandoning a
subgroup ahead of its buffered objects, and a forwarder cursor the writer can
read — and the draft-16 line is frozen upstream while development moves to
the next draft. A vendored copy takes those patches at near-zero rebase cost,
touches no CI lane and no release container, and keeps the wheel building
exactly as a third party would build it: `cargo` resolves the crate by path
like any other member.

## Drift guard — patches are commits, never in-place edits

`cargo xtask check-vendored-trees` pins one deterministic content hash for
this tree (recorded in `xtask/src/check_vendored_trees.rs`, run with the other
source-walking gates and by `cargo test -p xtask`). Any byte change — an edit,
a reformat, an added or removed file — fails with the drifted directory named.
Every patch below therefore lands as its own commit that also updates the
recorded hash; a change to this tree that does not show up in that hash is the
accident the guard exists to catch.

The tree is rustfmt-clean as vendored under its own `edition = "2021"`, so the
wheel's formatting gate is a no-op over it. `src/util/` is not reachable from
`src/lib.rs` and is not compiled; it is kept verbatim rather than trimmed.

## What the wheel's lanes do with it

- `cargo fmt --all --check` in the wheel directory covers the vendored tree.
- `cargo test --locked --lib` and `cargo clippy --locked --lib --all-targets`
  in the wheel directory select the root package alone. The vendored crate's
  own tests run with `cargo test -p moq-transport` from the same directory;
  the wheel's own tests pin every patched behaviour through the crate's public
  API, so the lane still protects the patches.
- The wheel's third-party notices attribute the crate as before; `cargo about`
  synthesises the MIT text from the manifest's SPDX expression, since the
  published crate ships no licence file of its own.

## License

The vendored crate is **MIT OR Apache-2.0**, with Cloudflare's
`SPDX-FileCopyrightText` / `SPDX-License-Identifier` headers on every source
file. This is one of the two deliberate exceptions to the repo-wide BUSL-1.1
header rule (the other is the vulkanalia fork): **do not add BUSL headers to
any file under `packages/streamlib-moq/vendor/moq-transport`**, and do not
"improve" the sources — the only edits it carries are the recorded patches
below. The exception is exact-dir: first-party code one path segment away in
the same wheel still carries BUSL.

## Provenance

- Upstream repo: `github.com/cloudflare/moq-rs`, crate directory
  `moq-transport`.
- Vendored release: crates.io `moq-transport 0.16.2`, published from upstream
  commit `66f27b87a639ca1b7a28acb46b1c864c2e374ff7` (`.cargo_vcs_info.json`,
  kept in the tree).
- Copied from cargo's registry source of that release, whole, minus two files:
  `.cargo-ok` (cargo's extraction marker) and the crate's own `Cargo.lock`
  (the wheel's lockfile is the one that resolves it). `Cargo.toml` is the
  registry-normalised manifest, unedited; `Cargo.toml.orig` is kept beside it
  as upstream wrote it.

## Local patches on top of the vendored release

Each patch is one commit in this repo, applies cleanly to upstream `main`
(which still carries the 0.16.2 sources), and is recorded here with where it
stands upstream.

1. **`SubgroupWriter::abandon` — a subgroup can be abandoned ahead of its
   buffered objects, with a draft-16 reset code.** Files: `src/serve/error.rs`,
   `src/serve/subgroup.rs`, `src/session/subscribed.rs`. Adds
   `ServeError::Abandoned(DataStreamResetCode)`; `SubgroupState.abandoned`;
   `SubgroupWriter::abandon(code)`; `SubgroupReader::next` returns the abandon
   ahead of every buffered object; `SubgroupReader::abandoned` and
   `until_abandoned`; the forwarder races each payload chunk write against
   `until_abandoned` and resets the QUIC stream with the abandon's own code
   (`reset_code_for`), counted as an expected shutdown rather than a failure.
   This is what makes `DataStreamResetCode::DeliveryTimeout` reachable: before
   it, `close` drained every written object and only then reset, so nothing a
   publisher could do took a stale backlog off the wire. Upstream: not filed.

## Re-vendor recipe

1. Fetch the new release's source (`cargo fetch` puts it under
   `~/.cargo/registry/src/<index>/moq-transport-<version>/`) or check out the
   upstream tag.
2. Replace the tree with it, minus `.cargo-ok` and `Cargo.lock`.
3. Re-apply the patches listed above, one commit each, or drop the ones the
   release has taken.
4. Update `version` on the wheel's `moq-transport` path dependency and run
   `cargo check` in the wheel directory so its lockfile follows.
5. Record the new release and upstream commit in the Provenance section.
6. Re-capture the drift-guard hash: run `cargo xtask check-vendored-trees` —
   it fails printing the new hash — and update `VENDORED_TREES` in
   `xtask/src/check_vendored_trees.rs` **in the same commit** as the
   re-vendor.
7. `cargo test -p moq-transport` and the wheel's own `cargo test --locked
   --lib`, `cargo fmt --all --check` and clippy, all from the wheel directory.
