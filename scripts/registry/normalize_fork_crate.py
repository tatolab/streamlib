#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Normalize a vulkanalia-fork `.crate` so the tatolab-registry URLs it embeds
# are the CANONICAL registry-index instead of the ephemeral serving-port URL
# that `cargo package` records. `cargo package` bakes the port it happened to
# resolve fork siblings from into two files inside the `.crate`, and both make
# the tarball's checksum port-coupled:
#
#   1. `<name>-<version>/Cargo.toml` — a fork-sibling dep (`registry = "tatolab"`)
#      is normalized into `registry-index = "sparse+http://127.0.0.1:PORT/cargo/"`.
#   2. `<name>-<version>/Cargo.lock` — the bundled lockfile records each
#      fork-sibling package with `source = "sparse+http://127.0.0.1:PORT/cargo/"`.
#      (The sibling checksums in it are already canonical, because each sibling
#      is normalized before the next crate is packaged.)
#
# Rewriting both to the canonical `sparse+https://registry.tatolab.com/cargo/`
# (the workspace's [registries.tatolab] index, identical to the committed
# lockfile `source`) makes the `.crate` a pure function of source content:
# port-independent and reproducible, so its checksum can be frozen in the
# committed root `Cargo.lock`.
#
# `vulkanalia-sys` has NO fork-sibling dep → no `registry-index` line and no
# `sparse+` source in its bundled lock → already byte-stable and cargo-native;
# this normalizer leaves it untouched (exit without writing) so its
# committed-lock checksum stays exactly as cargo emits it. crates.io sources
# (`registry+https://.../crates.io-index`, or a `sparse+https://index.crates.io/`)
# are never rewritten.
#
#   normalize_fork_crate.py <crate-path> <name> <version> <canonical_index_url>
#
# Behavior:
#   * Decode the `.crate` gzip → tar; read `<name>-<version>/Cargo.toml`.
#   * If it has no `registry-index = "sparse+..."` line, do nothing (exit 0) —
#     the crate has no fork-sibling dep and is already byte-stable.
#   * Otherwise rewrite the manifest's `registry-index` and the bundled
#     lockfile's fork `source` URLs to <canonical_index_url>, drop any
#     `.cargo_vcs_info.json` entry, re-tar (GNU format, original member order,
#     every header field cloned verbatim from cargo's output), and re-gzip as a
#     hand-framed gzip stream: a fixed 10-byte header (OS=0xff "unknown",
#     XFL=0, MTIME=0, no embedded filename) + a raw level-0 (STORED) DEFLATE
#     body + CRC32/ISIZE trailer. Overwrite the crate in place.
#
# The gzip output is byte-identical across operating systems, zlib versions,
# AND Python versions: the STORED DEFLATE framing is zlib-standardized, and the
# header/trailer are constructed by hand rather than by `gzip.compress` (whose
# OS byte is the platform's zlib OS_CODE — 0x03 on Linux, other values
# elsewhere — which would otherwise couple the frozen checksum to the emit
# host's OS). So the committed-lock checksum reproduces on any emit host.
#
# Idempotent: re-running on an already-normalized crate reproduces identical
# bytes (the URLs are already canonical, headers are preserved, the gzip framing
# is deterministic).

import io
import re
import struct
import sys
import tarfile
import zlib

# Fixed gzip header: magic, CM=deflate(8), FLG=0, MTIME=0 (4 bytes), XFL=0,
# OS=0xff ("unknown" — platform-independent, NOT the host's zlib OS_CODE).
GZIP_FIXED_HEADER = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff"


def gzip_stored_fixed(data: bytes) -> bytes:
    """Deterministic gzip of `data`: fixed header + raw STORED DEFLATE + trailer.

    Independent of OS, zlib version, and Python version (unlike
    `gzip.compress`, whose OS byte tracks the platform's zlib OS_CODE).
    """
    compressor = zlib.compressobj(0, zlib.DEFLATED, -zlib.MAX_WBITS)
    body = compressor.compress(data) + compressor.flush()
    trailer = struct.pack("<II", zlib.crc32(data) & 0xFFFFFFFF, len(data) & 0xFFFFFFFF)
    return GZIP_FIXED_HEADER + body + trailer


def gzip_decompress(blob: bytes) -> bytes:
    """Decompress a gzip stream (any header), returning the raw tar bytes."""
    return zlib.decompress(blob, zlib.MAX_WBITS | 16)

REGISTRY_INDEX_RE = re.compile(r'registry-index = "sparse\+[^"]*"')
# A bundled-lockfile fork source: any `sparse+` registry that is NOT crates.io's
# own sparse index. crates.io in these locks uses the git-index form
# (`registry+https://.../crates.io-index`), so this matches only the tatolab fork.
LOCK_SOURCE_RE = re.compile(r'source = "sparse\+(?!https://index\.crates\.io/)[^"]*"')


def normalize(crate_path: str, name: str, version: str, canonical_index: str) -> bool:
    manifest_member = f"{name}-{version}/Cargo.toml"
    lock_member = f"{name}-{version}/Cargo.lock"
    vcs_member = f"{name}-{version}/.cargo_vcs_info.json"

    with open(crate_path, "rb") as fh:
        raw = gzip_decompress(fh.read())

    with tarfile.open(fileobj=io.BytesIO(raw), mode="r") as src:
        members = src.getmembers()
        manifest_ti = next((m for m in members if m.name == manifest_member), None)
        if manifest_ti is None:
            # Not a manifest-bearing crate we recognize — leave untouched.
            return False

        manifest_text = src.extractfile(manifest_ti).read().decode("utf-8")
        if not REGISTRY_INDEX_RE.search(manifest_text):
            # No fork-sibling registry-index (e.g. vulkanalia-sys) — already
            # byte-stable and cargo-native; do not rewrite.
            return False

        # Cache every regular member's payload while the source archive is open.
        payloads = {
            m.name: src.extractfile(m).read() for m in members if m.isreg()
        }

    # Per-member body rewrites: manifest registry-index + bundled-lock fork source.
    rewritten = {
        manifest_member: REGISTRY_INDEX_RE.sub(
            f'registry-index = "{canonical_index}"',
            payloads[manifest_member].decode("utf-8"),
        ).encode("utf-8")
    }
    if lock_member in payloads:
        rewritten[lock_member] = LOCK_SOURCE_RE.sub(
            f'source = "{canonical_index}"',
            payloads[lock_member].decode("utf-8"),
        ).encode("utf-8")

    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=tarfile.GNU_FORMAT) as dst:
        for member in members:
            if member.name == vcs_member:
                # Drop the git-HEAD vcs-info entry if cargo ever emits one.
                continue
            # Reuse the source TarInfo verbatim (mtime / mode / uid / gid /
            # uname / gname / type all preserved) so only the rewritten bodies
            # change.
            if member.name in rewritten:
                body = rewritten[member.name]
                member.size = len(body)
                dst.addfile(member, io.BytesIO(body))
            elif member.isreg():
                dst.addfile(member, io.BytesIO(payloads[member.name]))
            else:
                dst.addfile(member)

    # Hand-framed STORED gzip: byte-identical regardless of OS / zlib / Python
    # version, so the emitted checksum reproduces on any emit host.
    gzipped = gzip_stored_fixed(out.getvalue())
    with open(crate_path, "wb") as fh:
        fh.write(gzipped)
    return True


def main() -> None:
    if len(sys.argv) != 5:
        sys.stderr.write(
            "usage: normalize_fork_crate.py <crate-path> <name> <version> "
            "<canonical_index_url>\n"
        )
        sys.exit(2)
    crate_path, name, version, canonical_index = sys.argv[1:5]
    normalize(crate_path, name, version, canonical_index)


if __name__ == "__main__":
    main()
