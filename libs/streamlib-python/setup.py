# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Codegen hook: regenerates `python/streamlib/_generated_/` from this
project's `streamlib.yaml` when building inside the monorepo.

The Python SDK is pure Python (`pyproject.toml` uses `setuptools.build_meta`).
Codegen invokes the `streamlib` CLI via `cargo run` in the workspace root
because pure-Python projects have no Cargo build chain of their own.

`_generated_/` is a build artifact, NOT part of the published source — the
distribution excludes it (see `MANIFEST.in`), exactly as a crate excludes
`target/` or an npm package excludes `dist` from its source tarball. Two build
contexts share this file:

- **In the monorepo** the workspace is reachable and `cargo` is on PATH, so
  codegen runs and materializes `_generated_/` for local editable installs /
  wheel builds.
- **Installing the published source on a host without the workspace** codegen
  is skipped — `import streamlib` works without `_generated_/` (the wire
  vocabulary is imported lazily). The runtime layer regenerates `_generated_/`
  into the venv after pulling the source down; a developer testing locally can
  also run codegen themselves.

The guard never fails the build for a missing `_generated_/`: source-only is a
valid installed state.
"""

import os
import shutil
import subprocess
from pathlib import Path

from setuptools import setup
from setuptools.command.build_py import build_py
from setuptools.command.develop import develop


ROOT = Path(__file__).parent.resolve()
WORKSPACE = ROOT.parent.parent
OUTPUT = ROOT / "python" / "streamlib" / "_generated_"


def _codegen_available() -> bool:
    """True only inside the monorepo: the workspace `Cargo.toml`, this
    project's `streamlib.yaml`, and `cargo` must all be reachable. False when
    installing a published sdist on a machine without the workspace."""
    return (
        (WORKSPACE / "Cargo.toml").exists()
        and (ROOT / "streamlib.yaml").exists()
        and shutil.which("cargo") is not None
    )


def regenerate():
    if not _codegen_available():
        # Published-source install path (no monorepo workspace): leave
        # `_generated_/` to the runtime layer, which regenerates it after
        # pulling the source down. `import streamlib` works without it (the
        # wire vocabulary is imported lazily), so a missing `_generated_/` is
        # a valid state, not a build failure. A pre-existing tree (local dev)
        # is left untouched.
        return

    if OUTPUT.exists():
        shutil.rmtree(OUTPUT)
    OUTPUT.mkdir(parents=True, exist_ok=True)

    subprocess.run(
        [
            "cargo",
            "run",
            "--release",
            "--quiet",
            "-p",
            "streamlib-cli",
            "--",
            "generate",
            "--runtime",
            "python",
            "--project-dir",
            str(ROOT),
            "--output",
            str(OUTPUT),
        ],
        cwd=str(WORKSPACE),
        check=True,
        env={**os.environ, "RUSTFLAGS": os.environ.get("RUSTFLAGS", "")},
    )


class BuildPy(build_py):
    def run(self):
        regenerate()
        super().run()


class Develop(develop):
    def run(self):
        regenerate()
        super().run()


setup(cmdclass={"build_py": BuildPy, "develop": Develop})
