#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Render ONE cargo sparse-index NDJSON line for a crate, from the normalized
# Cargo.toml embedded in its `.crate` tarball. The single source of truth for
# the index-line shape; the xtask closure emit shells out to it per crate.
#
# Env in:  NAME, VERSION, CKSUM (sha256 hex of the .crate), CRATE (path to it)
# stdout:  a single NDJSON line + trailing newline (cargo sparse index format).
#
# The cargo sparse index line schema (see the cargo registry-index docs):
#   {"name","vers","deps":[...],"cksum","features","yanked"}
# Each dep: {"name","req","features","optional","default_features","target",
#            "kind","registry","package"?}. A dep from THIS registry (a fork
# sibling or another closure crate) OMITS "registry"; a crates.io dep sets it
# to the canonical crates.io index URL so cargo fetches it from crates.io,
# not this tree.
#
# Same-registry detection is data-driven: `cargo package` normalizes a
# `registry = "tatolab"` dev dep into `registry-index = "<the index URL cargo
# resolved it from>"` in the packaged Cargo.toml. A dep carrying
# `registry-index` therefore resolved from THIS registry at package time
# (the packaging env points CARGO_REGISTRIES_TATOLAB_INDEX at the tree being
# built); a dep without it is crates.io. This covers the vulkanalia fork
# siblings AND every streamlib closure crate without a hardcoded name list.

import json
import os
import subprocess
import sys

# Cargo's default source for a dep with no explicit registry, when the crate
# itself lives in a non-crates.io registry, is still crates.io — named by this
# canonical index URL in the sparse index line.
CRATES_IO_INDEX = "https://github.com/rust-lang/crates.io-index"


def load_packaged_manifest(crate_path: str, name: str, version: str) -> dict:
    """Extract and parse `<name>-<version>/Cargo.toml` from the .crate gzip-tar."""
    member = f"{name}-{version}/Cargo.toml"
    out = subprocess.run(
        ["tar", "-xzOf", crate_path, member],
        check=True,
        capture_output=True,
    ).stdout
    try:
        import tomllib  # py3.11+
        return tomllib.loads(out.decode("utf-8"))
    except ModuleNotFoundError:
        import tomlkit
        return tomlkit.parse(out.decode("utf-8"))


def normalize_req(version: str) -> str:
    """A bare `X.Y.Z` req means `^X.Y.Z`; operators/ranges pass through."""
    v = str(version).strip()
    if not v:
        return "*"
    if v[0] in "^~=<>*" or "," in v or v == "*":
        return v
    return f"^{v}"


def dep_entries(manifest: dict) -> list:
    entries = []
    kinds = [
        ("dependencies", "normal"),
        ("build-dependencies", "build"),
        ("dev-dependencies", "dev"),
    ]
    # Plain tables plus `[target.<cfg>.dependencies]` variants.
    tables = []
    for key, kind in kinds:
        if key in manifest and isinstance(manifest[key], dict):
            tables.append((manifest[key], kind, None))
    for cfg, tbl in (manifest.get("target") or {}).items():
        if not isinstance(tbl, dict):
            continue
        for key, kind in kinds:
            if key in tbl and isinstance(tbl[key], dict):
                tables.append((tbl[key], kind, cfg))

    for table, kind, target in tables:
        for dep_name, spec in table.items():
            if isinstance(spec, str):
                spec = {"version": spec}
            elif not isinstance(spec, dict):
                continue
            # The published name is `package` when the dep is renamed.
            real_name = spec.get("package", dep_name)
            req = normalize_req(spec.get("version", "*"))
            features = list(spec.get("features", []))
            optional = bool(spec.get("optional", False))
            default_features = bool(spec.get("default-features", True))
            # Per the cargo index format, `name` is the name the depending
            # crate USES (the rename when `package = ...` is present — the
            # manifest's dep-table key), and `package` carries the real
            # published crate name. Feature references like `dep/feat` in the
            # crate's `[features]` resolve against `name`, so getting this
            # orientation wrong makes cargo reject the WHOLE index entry as
            # invalid (e.g. `std = ["vulkanalia-sys/std"]` with no dep NAMED
            # `vulkanalia-sys`).
            entry = {
                "name": dep_name,
                "req": req,
                "features": features,
                "optional": optional,
                "default_features": default_features,
                "target": target,
                "kind": kind,
            }
            # A crates.io dep names the crates.io index; a same-registry dep
            # (one the packaged toml records with `registry-index`) OMITS the
            # `registry` key entirely — cargo treats an absent key as "this
            # registry" (matches registry daemon's index output).
            if "registry-index" not in spec:
                entry["registry"] = CRATES_IO_INDEX
            # The real published crate name when the dep was renamed.
            if real_name != dep_name:
                entry["package"] = real_name
            entries.append(entry)
    return entries


def main() -> None:
    name = os.environ["NAME"]
    version = os.environ["VERSION"]
    cksum = os.environ["CKSUM"]
    crate = os.environ["CRATE"]

    manifest = load_packaged_manifest(crate, name, version)
    features = {}
    if isinstance(manifest.get("features"), dict):
        features = {k: list(v) for k, v in manifest["features"].items()}

    line = {
        "name": name,
        "vers": version,
        "deps": dep_entries(manifest),
        "cksum": cksum,
        "features": features,
        "yanked": False,
    }
    sys.stdout.write(json.dumps(line, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
