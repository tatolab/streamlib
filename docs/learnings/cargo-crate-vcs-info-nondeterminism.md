# `cargo package` is byte-deterministic EXCEPT `.cargo_vcs_info.json`

## Symptom

Emitting the static registry tree twice from the same source produces
**different `.crate` checksums for identical `(crate, version)`** — the
cargo sparse-index `cksum` line changes, and any consumer lockfile that
recorded the previous bytes churns. The crate source did not change; only
the checksum did. Nothing in the build log explains it.

## Root cause

`cargo package` is *almost* fully reproducible. On a fixed toolchain the
`.crate` (a gzip'd tar) has:

- gzip header MTIME zeroed (`1f 8b 08 08 00 00 00 00` — the four MTIME
  bytes are `0`),
- fixed tar entry mtimes / modes / uid / gid,
- stable (alphabetical) entry order,
- deterministic DEFLATE output.

Two `cargo package` runs of identical source differ in **exactly one
entry**: `{name}-{version}/.cargo_vcs_info.json`. When packaging from a
git checkout with commits, cargo embeds

```json
{ "git": { "sha1": "<git HEAD sha1>" }, "path_in_vcs": "" }
```

So the `.crate` checksum is a function of **git HEAD**, not of source. A
benign commit that doesn't touch a crate still moves HEAD, so the crate's
bytes — and its checksum — change on the next emit.

Empirically verified (throwaway crate, two commits, identical source):
the two `.crate`s differ starting at byte 33 (inside the first tar entry,
`.cargo_vcs_info.json`); decompress both, drop that one entry, and the
remaining tar bytes are byte-identical.

**No stable cargo flag suppresses vcs-info emission.** `--allow-dirty`
governs the *dirty-tree check*, not whether vcs-info is written; there is
no `--no-vcs-info` on stable. Stripping the entry post-hoc is the fix.

## Fix

Normalize the `.crate` after `cargo package`: gzip-decode, drop the
`{name}-{version}/.cargo_vcs_info.json` tar entry, re-tar the survivors
(cloning cargo's existing headers verbatim so their canonical mtime /
mode / uid / gid and GNU long-name handling are inherited, not
synthesized), and re-gzip with a fixed header (MTIME 0, no embedded
filename). The result is a pure function of source content.

The normalize step must be **idempotent** — a registry emit reuses a
previously-packaged `.crate` from `target/package/` across runs, so
normalize re-runs on an already-stripped crate and must reproduce
identical bytes. Cloning cargo's headers and re-emitting through the same
`tar` writer each time makes it a fixed point.

Two consequences worth pinning with tests:

- Two crates with identical source but different git HEAD sha1 normalize
  to byte-identical `.crate`s (equal checksum) — the byte-stability
  contract.
- A source-content fingerprint used for an immutability guard should hash
  the *canonical uncompressed* tar (not the gzip'd `.crate`), so it's
  independent of the gzip level too — a future flate2 bump shifts the
  emitted `.crate` bytes uniformly but must not falsely trip the guard.

Stripping `.cargo_vcs_info.json` loses git-provenance metadata, but it is
informational only (no consumer requires it), and reproducible-build
tooling routinely strips it for exactly this reason.

## Reference

- Implementation: `libs/streamlib-pack/src/crate_tarball.rs`
  (`normalize_crate_tarball`, `crate_content_fingerprint`,
  `finalize_crate_tarball`); wired into `emit_cargo_closure` in
  `libs/streamlib-pack/src/static_registry.rs`.
- Architecture: `docs/architecture/static-registry.md` (the
  atomic-release / byte-stable-emission section).
- Sibling concern: pypi sdist (`uv build --sdist`) and npm tgz
  (`deno pack`) almost certainly carry their own reproducibility vectors
  (file mtimes) and are candidates for the same immutable-per-version
  normalization — verify before assuming they're stable.
