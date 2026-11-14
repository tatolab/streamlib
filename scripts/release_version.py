# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The version each merge to main releases as, and the manifests that carry it.

Every merge releases. The patch moves by default; the minor moves when the
control contract's `x-contract-version` changed since the last release, or when
a release is dispatched with `bump: minor`; the major moves only when a release
is dispatched with `bump: major`. A version in the tree newer than the last
release is taken as it stands.

Usage:
  release_version.py next --last-release X.Y.Z --tree X.Y.Z --bump patch|minor|major
  release_version.py apply X.Y.Z
  release_version.py tree

Stdlib only: the release job runs it on a runner with no dependencies installed.
"""

import argparse
import json
import re
import sys
from pathlib import Path

REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
RELEASE_VERSION_FILES_PATH = REPOSITORY_ROOT / "release-version-files.json"
WORKSPACE_MANIFEST_RELATIVE_PATH = "Cargo.toml"
SEMVER_PATTERN = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")
BUMPS = ("patch", "minor", "major")


def semver_parts(version):
    match = SEMVER_PATTERN.match(version)
    if match is None:
        raise ValueError(f"not X.Y.Z: {version}")
    return tuple(int(part) for part in match.groups())


def next_release_version(last_release_version, tree_version, bump):
    """The version this release takes, newer than `last_release_version`."""
    if bump not in BUMPS:
        raise ValueError(f"not one of {BUMPS}: {bump}")
    if last_release_version is None:
        return tree_version
    major, minor, patch = semver_parts(last_release_version)
    if bump == "major":
        bumped = (major + 1, 0, 0)
    elif bump == "minor":
        bumped = (major, minor + 1, 0)
    else:
        bumped = (major, minor, patch + 1)
    return ".".join(str(part) for part in max(bumped, semver_parts(tree_version)))


def release_version_files():
    """Each manifest that carries the one version: `{path, table}` (TOML), `{path, key}` (JSON),
    or `{path, marked_lines}`, every X.Y.Z on a line ending in that marker comment."""
    return json.loads(RELEASE_VERSION_FILES_PATH.read_text(encoding="utf-8"))


def toml_table_version_pattern(table):
    return re.compile(
        rf'(^\[{re.escape(table)}\][^\[]*?^version\s*=\s*")([^"]+)(")',
        re.MULTILINE | re.DOTALL,
    )


def tree_version():
    """`[workspace.package] version` in the workspace manifest."""
    manifest_text = (REPOSITORY_ROOT / WORKSPACE_MANIFEST_RELATIVE_PATH).read_text(encoding="utf-8")
    match = toml_table_version_pattern("workspace.package").search(manifest_text)
    if match is None:
        raise ValueError(f"{WORKSPACE_MANIFEST_RELATIVE_PATH} has no [workspace.package] version")
    return match.group(2)


def write_version_into(version_file, version):
    path = REPOSITORY_ROOT / version_file["path"]
    text = path.read_text(encoding="utf-8")
    if "marked_lines" in version_file:
        marked_line_pattern = re.compile(rf"^.*#\s*{re.escape(version_file['marked_lines'])}\s*$", re.MULTILINE)
        replaced_text, marked_line_count = marked_line_pattern.subn(
            lambda match: re.sub(r"\d+\.\d+\.\d+", version, match.group(0)), text
        )
        if marked_line_count == 0:
            raise ValueError(f"{version_file['path']} has no line marked {version_file['marked_lines']}")
        path.write_text(replaced_text, encoding="utf-8")
        return
    if "table" in version_file:
        pattern = toml_table_version_pattern(version_file["table"])
    else:
        pattern = re.compile(rf'("{re.escape(version_file["key"])}"\s*:\s*")([^"]+)(")')
    replaced_text, replacement_count = pattern.subn(lambda match: match.group(1) + version + match.group(3), text, count=1)
    if replacement_count != 1:
        raise ValueError(f"{version_file['path']} holds no version where release-version-files.json says")
    path.write_text(replaced_text, encoding="utf-8")


def main(arguments):
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    next_command = commands.add_parser("next")
    next_command.add_argument("--last-release", default="")
    next_command.add_argument("--tree", required=True)
    next_command.add_argument("--bump", choices=BUMPS, required=True)
    apply_command = commands.add_parser("apply")
    apply_command.add_argument("version")
    commands.add_parser("tree")
    parsed = parser.parse_args(arguments)

    if parsed.command == "next":
        print(next_release_version(parsed.last_release or None, parsed.tree, parsed.bump))
    elif parsed.command == "apply":
        semver_parts(parsed.version)
        for version_file in release_version_files():
            write_version_into(version_file, parsed.version)
    else:
        print(tree_version())
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
