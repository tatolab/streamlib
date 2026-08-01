# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Make the #1702 subprocess baseline arm runnable, deterministically.

Three prerequisites the harness cannot satisfy from inside a measurement run,
each of which fails in a way that is easy to misread as a slow baseline rather
than a broken one:

1. ``libstreamlib_python_native.so`` must exist. Without it every subprocess
   dies at load and the arm reports zero frames.
2. The baseline package's ``streamlib`` dependency resolves from no index —
   ``uv`` reports "not found in the package registry". A path override at this
   checkout's Python SDK is injected into a staged copy of the package, which
   is the same rewrite ``streamlib link --engine`` performs
   (``python_venv.rs:360-393``), scoped here so a measurement run neither
   depends on nor disturbs global link state.
3. The staged copy has to sit in a ``streamlib_modules/@spike/...`` slot for
   the module loader to resolve it.

Run this before any ``--mode subprocess`` cell. It is idempotent.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

PACKAGE_ORG = "spike"
PACKAGE_NAME = "pyembed-subprocess-baseline"
PACKAGE_SOURCE_DIRECTORY_NAME = "subprocess_baseline_package"

PYTHON_NATIVE_LIBRARY_FILE_NAME = "libstreamlib_python_native.so"
PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE = "STREAMLIB_PYTHON_NATIVE_LIB"

# Points the module loader's `streamlib_modules/` lookup at the spike crate
# instead of the checkout root (`streamlib_home.rs:116,148`). Without it the
# link, the lock file, and the module slot all land at the repo root, which the
# spike has no business writing to.
APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_MODULES_DIR"


def resolve_spike_crate_root() -> Path:
    return Path(__file__).resolve().parent.parent


def resolve_engine_checkout_root() -> Path:
    """The repo root — the first ancestor carrying both `packages/` and `sdk/`.

    Derived rather than configured so a worktree copy of the spike provisions
    against its own checkout instead of whichever one happens to be linked.
    """
    for candidate in resolve_spike_crate_root().parents:
        if (candidate / "packages").is_dir() and (candidate / "sdk").is_dir():
            return candidate
    raise RuntimeError(
        "cannot locate the engine checkout root above "
        f"{resolve_spike_crate_root()} — expected an ancestor with packages/ and sdk/"
    )


def build_python_native_cdylib(engine_checkout_root: Path, release: bool) -> Path:
    """Build the python-native cdylib and return the artifact path.

    Pinned by absolute path rather than left to a search: a stale artifact from
    an earlier build would otherwise win silently and the baseline arm would
    measure a cdylib that is not the one under test.
    """
    profile_arguments = ["--release"] if release else []
    subprocess.run(
        ["cargo", "build", *profile_arguments, "-p", "streamlib-python-native"],
        cwd=engine_checkout_root,
        check=True,
    )
    artifact = (
        engine_checkout_root
        / "target"
        / ("release" if release else "debug")
        / PYTHON_NATIVE_LIBRARY_FILE_NAME
    )
    if not artifact.is_file():
        raise RuntimeError(
            f"cargo reported success but {artifact} is absent — the baseline arm "
            "cannot run without the python-native cdylib"
        )
    return artifact


def stage_baseline_package_with_sdk_path_override(
    engine_checkout_root: Path,
) -> Path:
    """Copy the package source to a gitignored slot, injecting the SDK path.

    The override carries an absolute path, so the staged copy stays valid
    wherever the build orchestrator relocates it.
    """
    source_directory = resolve_spike_crate_root() / PACKAGE_SOURCE_DIRECTORY_NAME
    if not (source_directory / "streamlib.yaml").is_file():
        raise RuntimeError(f"no package source at {source_directory}")

    staged_directory = (
        resolve_spike_crate_root() / ".provisioned" / PACKAGE_NAME
    )
    if staged_directory.exists():
        shutil.rmtree(staged_directory)
    staged_directory.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(
        source_directory,
        staged_directory,
        ignore=shutil.ignore_patterns("__pycache__", ".venv", "*.pyc"),
    )

    python_sdk_path = engine_checkout_root / "sdk" / "streamlib-python"
    if not (python_sdk_path / "setup.py").is_file():
        raise RuntimeError(f"no Python SDK at {python_sdk_path}")

    pyproject_path = staged_directory / "pyproject.toml"
    pyproject_body = pyproject_path.read_text()
    declares_a_uv_sources_table = any(
        line.strip() == "[tool.uv.sources]"
        for line in pyproject_body.splitlines()
        if not line.lstrip().startswith("#")
    )
    if declares_a_uv_sources_table:
        raise RuntimeError(
            "the committed pyproject already carries a [tool.uv.sources] table; "
            "the override would be ambiguous"
        )
    pyproject_path.write_text(
        pyproject_body
        + "\n[tool.uv.sources]\n"
        + f'streamlib = {{ path = "{python_sdk_path}", editable = true }}\n'
    )
    return staged_directory


def link_staged_package_into_app_modules(
    application_modules_root: Path,
    staged_directory: Path,
    streamlib_executable: Path,
) -> Path:
    """Symlink the staged package into `streamlib_modules/@spike/<name>`.

    `link` rather than `add`/`install`: those two take finalized artifacts, and
    this is a local checkout that changes between runs.
    """
    application_modules_root.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            str(streamlib_executable),
            "link",
            str(staged_directory),
            "--dir",
            str(application_modules_root),
        ],
        cwd=application_modules_root,
        check=True,
    )
    slot = (
        application_modules_root
        / "streamlib_modules"
        / f"@{PACKAGE_ORG}"
        / PACKAGE_NAME
    )
    if not slot.exists():
        raise RuntimeError(f"streamlib link reported success but {slot} is absent")
    return slot


def provision_package_venv(staged_directory: Path) -> Path:
    """Create the package's `.venv`, which is what makes the slot loadable.

    `streamlib install` skips linked entries ("linked entries stay
    lazy/edit-rebuild", `install.rs:21-22`), and the module loader refuses to
    cold-build an installed slot — it demands `.venv/bin/python` and returns
    `InstalledPackageNotBuilt` otherwise (`source.rs:677-701`). Nothing in
    between provisions a linked Python package, so this mirrors the two steps
    the build orchestrator would have run (`python_venv.rs:93-147`): `uv venv`,
    then a source install of the package itself.

    The SDK arrives editable from this checkout, whose `streamlib/_generated_`
    tree is already populated, so the orchestrator's codegen step
    (`ensure_streamlib_generated_in_venv`) has nothing left to do; it is
    verified rather than repeated.
    """
    venv_directory = staged_directory / ".venv"
    subprocess.run(["uv", "venv", str(venv_directory)], check=True)
    venv_python = venv_directory / "bin" / "python"
    subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            str(venv_python),
            str(staged_directory),
        ],
        check=True,
    )
    if not venv_python.is_file():
        raise RuntimeError(f"uv reported success but {venv_python} is absent")

    generated_probe = subprocess.run(
        [
            str(venv_python),
            "-c",
            "import streamlib._generated_ as g; print(g.__file__)",
        ],
        capture_output=True,
        text=True,
    )
    if generated_probe.returncode != 0:
        raise RuntimeError(
            "the provisioned venv has no populated streamlib._generated_ tree, so "
            "the subprocess would fail at import rather than run slowly: "
            f"{generated_probe.stderr.strip()}"
        )
    return venv_python


def resolve_streamlib_executable(engine_checkout_root: Path, release: bool) -> Path:
    executable = (
        engine_checkout_root
        / "target"
        / ("release" if release else "debug")
        / "streamlib"
    )
    if not executable.is_file():
        raise RuntimeError(
            f"{executable} is absent — build it with "
            f"`cargo build {'--release ' if release else ''}-p streamlib-cli`"
        )
    return executable


def provision(release: bool) -> dict:
    engine_checkout_root = resolve_engine_checkout_root()
    application_modules_root = resolve_spike_crate_root()
    native_library = build_python_native_cdylib(engine_checkout_root, release)
    staged_directory = stage_baseline_package_with_sdk_path_override(
        engine_checkout_root
    )
    venv_python = provision_package_venv(staged_directory)
    slot = link_staged_package_into_app_modules(
        application_modules_root,
        staged_directory,
        resolve_streamlib_executable(engine_checkout_root, release),
    )
    return {
        "engine_checkout_root": str(engine_checkout_root),
        "python_native_library_path": str(native_library),
        "python_native_library_environment_variable": (
            PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE
        ),
        "application_modules_root": str(application_modules_root),
        "application_modules_root_environment_variable": (
            APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE
        ),
        "staged_package_directory": str(staged_directory),
        "package_venv_python": str(venv_python),
        "linked_module_slot": str(slot),
        "processor_type_reference": (
            f"@{PACKAGE_ORG}/{PACKAGE_NAME}/PyembedSubprocessBaselineStage"
        ),
    }


def main() -> int:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument(
        "--debug",
        action="store_true",
        help="provision against the debug profile instead of release",
    )
    arguments = argument_parser.parse_args()

    record = provision(release=not arguments.debug)
    os.environ[PYTHON_NATIVE_LIBRARY_ENVIRONMENT_VARIABLE] = record[
        "python_native_library_path"
    ]
    os.environ[APP_MODULES_DIRECTORY_ENVIRONMENT_VARIABLE] = record[
        "application_modules_root"
    ]
    sys.stdout.write(json.dumps(record, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
