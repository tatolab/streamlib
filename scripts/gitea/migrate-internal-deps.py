#!/usr/bin/env python3
"""Migrate internal cross-crate path deps to the canonical
``{ path, version, registry = "gitea" }`` form — the standard cargo
"publish a workspace" pattern (tokio et al.).

What it does:

* Targets ``[dependencies]``, ``[build-dependencies]`` and their
  ``[target.<cfg>.dependencies]`` / ``[target.<cfg>.build-dependencies]``
  variants in every member ``Cargo.toml``.
* **Skips** ``[dev-dependencies]`` (and target dev-deps): cargo strips
  bare-path dev-deps on publish, and annotating them creates publish-order
  cycles — e.g. ``streamlib-engine`` dev-deps ``streamlib``.
* ``version`` is read from each *target* crate's own ``[package].version``
  (resolving ``version.workspace = true`` against the workspace root), so a
  crate off the shared version (e.g. ``streamlib-cross-rustc-fixture`` at
  ``0.1.0``) gets its real version, not a blanket stamp.
* ``registry = "gitea"`` is required — without it cargo records the dep as
  crates.io and ``cargo publish`` fails.
* Inline tables are **rebuilt fresh** — appending keys in place corrupts the
  ``,`` separators tomlkit tracks. Key order is ``package?, path, version,
  registry, <rest>``.

The ``path`` stays: it is a dev-only affordance cargo strips from the
published manifest, so the monorepo keeps building in place (path wins
locally) while consumers see only ``version`` + ``registry``.

Idempotent. ``--check`` reports files that would change and exits non-zero if
any do (CI drift guard). Needs ``tomlkit``.

Usage:
    python3 scripts/gitea/migrate-internal-deps.py [--check] [ROOT]
"""

from __future__ import annotations

import sys
from pathlib import Path

import tomlkit
from tomlkit.items import InlineTable

DEP_SECTIONS = ("dependencies", "build-dependencies")
REGISTRY = "gitea"


def workspace_root(start: Path) -> Path:
    """Walk up from *start* to the dir whose Cargo.toml declares [workspace]."""
    cur = start.resolve()
    for cand in [cur, *cur.parents]:
        manifest = cand / "Cargo.toml"
        if manifest.is_file():
            try:
                doc = tomlkit.parse(manifest.read_text())
            except Exception:
                continue
            if "workspace" in doc:
                return cand
    raise SystemExit(f"no workspace root found above {start}")


def crate_version(manifest_dir: Path, ws_version: str, _cache: dict) -> str | None:
    """[package].version of the crate at *manifest_dir*, resolving workspace inheritance."""
    key = manifest_dir.resolve()
    if key in _cache:
        return _cache[key]
    manifest = key / "Cargo.toml"
    if not manifest.is_file():
        _cache[key] = None
        return None
    pkg = tomlkit.parse(manifest.read_text()).get("package", {})
    ver = pkg.get("version")
    # version.workspace = true  →  inline table {"workspace": True}
    if isinstance(ver, dict) and ver.get("workspace"):
        resolved = ws_version
    elif isinstance(ver, str):
        resolved = ver
    else:
        resolved = None
    _cache[key] = resolved
    return resolved


def _render_value(v) -> str:
    """Serialize a single TOML value the way tomlkit would (quoted strings,
    true/false, arrays) — used to assemble a padded inline-table fragment."""
    doc = tomlkit.document()
    doc["_"] = v
    return tomlkit.dumps(doc).split("=", 1)[1].strip()


def rebuild_dep(name: str, orig: InlineTable, version: str) -> InlineTable:
    """Fresh inline table in canonical order: package?, path, version,
    registry, <rest>. Built by parsing a padded fragment so the result
    matches the repo's ``{ key = val }`` style instead of tomlkit's
    unpadded programmatic default."""
    pairs = []
    if "package" in orig:
        pairs.append(("package", orig["package"]))
    pairs.append(("path", orig["path"]))
    pairs.append(("version", version))
    pairs.append(("registry", REGISTRY))
    for k, v in orig.items():
        if k in ("package", "path", "version", "registry"):
            continue
        pairs.append((k, v))
    inner = ", ".join(f"{k} = {_render_value(v)}" for k, v in pairs)
    return tomlkit.parse(f"dep = {{ {inner} }}")["dep"]


def migrate_section(section, manifest_dir: Path, ws_version: str, vcache: dict) -> list[str]:
    changed = []
    for name in list(section.keys()):
        dep = section[name]
        if not isinstance(dep, (dict, InlineTable)):
            continue
        if "path" not in dep:
            continue
        target_dir = (manifest_dir / dep["path"]).resolve()
        version = crate_version(target_dir, ws_version, vcache)
        if version is None:
            print(f"  WARN {name}: no version at {target_dir}, skipped", file=sys.stderr)
            continue
        if dep.get("version") == version and dep.get("registry") == REGISTRY:
            continue  # already canonical
        section[name] = rebuild_dep(name, dep, version)
        changed.append(name)
    return changed


def iter_dep_sections(doc):
    """Yield (section_table, label) for every non-dev dependency table."""
    for sec in DEP_SECTIONS:
        if sec in doc:
            yield doc[sec], sec
    target = doc.get("target")
    if target:
        for cfg, tbl in target.items():
            for sec in DEP_SECTIONS:
                if sec in tbl:
                    yield tbl[sec], f"target.{cfg}.{sec}"


def main() -> int:
    args = [a for a in sys.argv[1:]]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    root = Path(args[0]) if args else Path(__file__).resolve().parents[2]
    root = workspace_root(root)
    ws_version = tomlkit.parse((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    print(f"workspace root: {root}  (version {ws_version})")

    vcache: dict = {}
    dirty = []
    for sub in ("libs", "packages", "examples"):
        for manifest in sorted((root / sub).rglob("Cargo.toml")):
            if "target" in manifest.parts:
                continue
            doc = tomlkit.parse(manifest.read_text())
            changed = []
            for section, _label in iter_dep_sections(doc):
                changed += migrate_section(section, manifest.parent, ws_version, vcache)
            if changed:
                rel = manifest.relative_to(root)
                dirty.append(rel)
                print(f"  {rel}: {', '.join(sorted(set(changed)))}")
                if not check:
                    manifest.write_text(tomlkit.dumps(doc))

    if check and dirty:
        print(f"\n{len(dirty)} manifest(s) would change — run without --check", file=sys.stderr)
        return 1
    print(f"\n{'would migrate' if check else 'migrated'} {len(dirty)} manifest(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
