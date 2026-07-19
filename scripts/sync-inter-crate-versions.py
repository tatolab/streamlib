#!/usr/bin/env python3
"""Sync inter-crate dependency version requirements to the workspace version.

The `simple` release-please release-type bumps `[workspace.package].version` in
the root Cargo.toml but ships no cargo dependency-requirement updater. Every
in-tree crate that depends on a sibling workspace-versioned crate pins it with a
frozen `version = "X.Y.Z"` requirement (alongside a dev-loop `path`). Those
requirements never move on their own: they stay caret-compatible within a
0.7.x line but silently EXCLUDE the next breaking bump (e.g. 0.8.0), which would
make the whole tree fail to resolve after a release.

This script rewrites, in place, every dependency requirement that targets a
workspace-versioned sibling crate (any crate whose own `[package]` uses
`version.workspace = true` — the engine/SDK/adapter crates plus vulkan-jpeg) to
the current `[workspace.package].version`. Independently-versioned domain
packages under `packages/*` (which set an explicit `version = "1.0.x"`) are NOT
workspace-versioned, so deps on them (e.g. streamlib-api-server, streamlib-audio)
are left untouched — their requirements are managed with the package, not the
workspace.

The rewrite is idempotent: a no-op when everything is already in sync (exit 0,
nothing written). Run from CI on the release-PR branch so the squash-merge folds
it into the release commit, and as a one-time sweep by hand.

Usage: sync-inter-crate-versions.py [REPO_ROOT]   (defaults to CWD)
"""

import re
import sys
from pathlib import Path

# `NAME = { ... }` inline-table dep, or `NAME = "req"` bare dep, at line start.
DEP_LINE = re.compile(r'^(?P<name>[A-Za-z0-9_-]+)\s*=\s*(?P<rhs>.+)$')
# `version = "req"` inside an inline table.
INLINE_VERSION = re.compile(r'(version\s*=\s*")(?P<req>[^"]*)(")')
# Whole-value bare string requirement: `= "0.7.0"`.
BARE_VERSION = re.compile(r'^"(?P<req>[^"]*)"\s*$')


def read_package_name_and_workspace_version(manifest_text: str) -> tuple[str | None, bool]:
    """Return (package name, uses_version_workspace) for a manifest's own crate."""
    in_package = False
    name: str | None = None
    version_is_workspace = False
    for raw in manifest_text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_package = line == "[package]"
            continue
        if not in_package:
            continue
        if name is None:
            m = re.match(r'name\s*=\s*"([^"]+)"', line)
            if m:
                name = m.group(1)
        if re.match(r'version\s*\.\s*workspace\s*=\s*true', line) or re.match(
            r'version\s*=\s*\{\s*workspace\s*=\s*true', line
        ):
            version_is_workspace = True
    return name, version_is_workspace


def read_workspace_version(root_manifest_text: str) -> str:
    """Read `[workspace.package].version` from the root manifest."""
    in_ws_package = False
    for raw in root_manifest_text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_ws_package = line == "[workspace.package]"
            continue
        if in_ws_package:
            m = re.match(r'version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    raise SystemExit("error: [workspace.package].version not found in root Cargo.toml")


def rewrite_manifest(text: str, ws_crates: set[str], ws_version: str) -> tuple[str, list[tuple[str, str]]]:
    """Rewrite dep requirements on workspace-versioned crates. Returns (text, changes)."""
    changes: list[tuple[str, str]] = []
    out_lines: list[str] = []
    for raw in text.splitlines():
        m = DEP_LINE.match(raw)
        if not m or m.group("name") not in ws_crates:
            out_lines.append(raw)
            continue
        name = m.group("name")
        rhs = m.group("rhs")
        bare = BARE_VERSION.match(rhs)
        if bare:
            if bare.group("req") != ws_version:
                changes.append((f"{name} (bare)", f'{bare.group("req")} -> {ws_version}'))
                raw = raw[: m.start("rhs")] + f'"{ws_version}"'
            out_lines.append(raw)
            continue
        vm = INLINE_VERSION.search(rhs)
        if vm and vm.group("req") != ws_version:
            changes.append((name, f'{vm.group("req")} -> {ws_version}'))
            new_rhs = rhs[: vm.start()] + vm.group(1) + ws_version + vm.group(3) + rhs[vm.end():]
            raw = raw[: m.start("rhs")] + new_rhs
        out_lines.append(raw)
    trailing_newline = "\n" if text.endswith("\n") else ""
    return "\n".join(out_lines) + trailing_newline, changes


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.cwd()
    root_manifest = root / "Cargo.toml"
    ws_version = read_workspace_version(root_manifest.read_text())

    manifests = [
        p
        for p in root.rglob("Cargo.toml")
        if "vendor" not in p.parts and "target" not in p.parts
    ]

    ws_crates: set[str] = set()
    for manifest in manifests:
        name, is_ws = read_package_name_and_workspace_version(manifest.read_text())
        if name and is_ws:
            ws_crates.add(name)

    total_changes = 0
    for manifest in sorted(manifests):
        text = manifest.read_text()
        new_text, changes = rewrite_manifest(text, ws_crates, ws_version)
        if changes:
            manifest.write_text(new_text)
            rel = manifest.relative_to(root)
            for dep, delta in changes:
                print(f"{rel}: {dep}: {delta}")
            total_changes += len(changes)

    if total_changes == 0:
        print(f"inter-crate versions already in sync at {ws_version}")
    else:
        print(f"synced {total_changes} inter-crate requirement(s) to {ws_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
