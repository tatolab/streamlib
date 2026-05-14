# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Codegen hook: regenerates `python/streamlib/_generated_/` from this
project's `streamlib.yaml` before setuptools builds the package.

The Python SDK is pure Python (`pyproject.toml` uses `setuptools.build_meta`).
Codegen invokes the `streamlib` CLI via `cargo run` in the workspace root
because pure-Python projects have no Cargo build chain of their own.
Contributors who skip this step (e.g. by deleting `python/streamlib/_generated_/`
without re-installing) get a clear `ModuleNotFoundError` at import time.
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


def regenerate():
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
