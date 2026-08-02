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
    """Copy the package source to a slot under target/, injecting the SDK path.

    The override carries an absolute path, so the staged copy stays valid
    wherever the build orchestrator relocates it.
    """
    source_directory = resolve_spike_crate_root() / PACKAGE_SOURCE_DIRECTORY_NAME
    if not (source_directory / "streamlib.yaml").is_file():
        raise RuntimeError(f"no package source at {source_directory}")

    staged_directory = provisioned_package_root() / PACKAGE_NAME
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


def probe_embedded_python_runtime(spike_crate_root: Path) -> dict:
    """Ask the harness which interpreter it actually embeds.

    Asked rather than inferred. `ldd` yields only `3.12`, and `uv venv --python
    3.12` is then free to resolve its own managed build: this rig paired an
    embedded CPython 3.12.3 (Ubuntu/GCC) with a baseline venv on 3.12.13
    (python-build-standalone/Clang) — a different compiler, ten patch releases
    apart — while a major.minor comparison called them the same runtime.
    """
    harness_binary = spike_crate_root / "target" / "release" / "tier_a_harness"
    if not harness_binary.is_file():
        raise RuntimeError(
            f"{harness_binary} is absent; build it before provisioning so the "
            "baseline venv can be pinned to the interpreter it embeds"
        )
    report_path = provisioned_package_root() / "embedded-python-runtime.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            str(harness_binary),
            "--report-embedded-python-runtime-to",
            str(report_path),
        ],
        check=True,
        capture_output=True,
    )
    return json.loads(report_path.read_text())


def resolve_interpreter_executable_for_runtime(embedded_runtime: dict) -> str:
    """The executable for the embedded interpreter, from its own `base_prefix`."""
    candidate = (
        Path(embedded_runtime["base_prefix"])
        / "bin"
        / f"python{embedded_runtime['major_minor']}"
    )
    if not candidate.is_file():
        raise RuntimeError(
            f"the harness embeds CPython {embedded_runtime['version']} with base_prefix "
            f"{embedded_runtime['base_prefix']}, but {candidate} does not exist, so the "
            "baseline venv cannot be pinned to the same interpreter"
        )
    return str(candidate)


def assert_arms_share_a_runtime(venv_python: Path, embedded_runtime: dict) -> None:
    """Refuse a provisioning whose two arms would run different Pythons.

    Compares the full `major.minor.micro` and the build banner, not just the
    minor version: two CPython 3.12 builds from different compilers ten patch
    releases apart are not the same runtime, and a guard that says they are
    reproduces the confound it was written to prevent. numpy is compared
    exactly, and an absent version on either side is a refusal rather than a
    skipped check.
    """
    reported = subprocess.run(
        [
            str(venv_python),
            "-c",
            "import sys, json;\n"
            "v = sys.version_info;\n"
            "try:\n"
            "    import numpy; n = numpy.__version__\n"
            "except ImportError:\n"
            "    n = None\n"
            "print(json.dumps({'version': f'{v.major}.{v.minor}.{v.micro}', "
            "'version_banner': sys.version, 'numpy_version': n}))",
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    venv_runtime = json.loads(reported)

    for field, description in (
        ("version", "CPython version"),
        ("version_banner", "CPython build"),
        ("numpy_version", "numpy version"),
    ):
        embedded_value = embedded_runtime.get(field)
        venv_value = venv_runtime.get(field)
        if embedded_value is None or venv_value is None:
            raise RuntimeError(
                f"the {description} is unknown on one of the arms "
                f"(in-process {embedded_value!r}, baseline {venv_value!r}); the arms "
                "cannot be shown to share a runtime, so they are not comparable"
            )
        if embedded_value != venv_value:
            raise RuntimeError(
                f"the arms disagree on {description}: the in-process arm runs "
                f"{embedded_value!r} and the baseline venv runs {venv_value!r}; every "
                "reported difference between them would include that delta"
            )


def provision_package_venv(staged_directory: Path, spike_crate_root: Path) -> Path:
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
    embedded_runtime = probe_embedded_python_runtime(spike_crate_root)
    embedded_interpreter_executable = resolve_interpreter_executable_for_runtime(
        embedded_runtime
    )
    venv_directory = staged_directory / ".venv"
    subprocess.run(
        [
            "uv",
            "venv",
            "--python",
            embedded_interpreter_executable,
            str(venv_directory),
        ],
        check=True,
    )
    venv_python = venv_directory / "bin" / "python"

    # numpy pinned to the in-process arm's own version, taken from the embedded
    # interpreter rather than from a `python3.X` on PATH: the realistic stage's
    # cost is numpy's, so two versions make the stage-callback column a
    # comparison of numpy releases.
    in_process_numpy_version = embedded_runtime.get("numpy_version")
    if in_process_numpy_version is None:
        raise RuntimeError(
            "the embedded interpreter has no numpy, so the baseline cannot be pinned "
            "to the in-process arm's version and the arms would not be comparable"
        )
    subprocess.run(
        [
            "uv",
            "pip",
            "install",
            "--python",
            str(venv_python),
            f"numpy=={in_process_numpy_version}",
            str(staged_directory),
        ],
        check=True,
    )
    if not venv_python.is_file():
        raise RuntimeError(f"uv reported success but {venv_python} is absent")

    assert_arms_share_a_runtime(venv_python, embedded_runtime)

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
    venv_python = provision_package_venv(staged_directory, application_modules_root)
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
        "embedded_python_runtime": probe_embedded_python_runtime(
            application_modules_root
        ),
        "linked_module_slot": str(slot),
        "processor_type_reference": (
            f"@{PACKAGE_ORG}/{PACKAGE_NAME}/PyembedSubprocessBaselineStage"
        ),
    }


def provisioned_package_root() -> Path:
    """Where the staged package and its venv live.

    Under `target/` rather than a dot-directory at the crate root because the
    repo's `check-manifest-schema` walk skips `target/`, `node_modules/`,
    `.git/` and `.streamlib/` and nothing else (`xtask/src/manifest_schema.rs:66`).
    A staged copy anywhere else puts both its own manifest and the `streamlib.yaml`
    vendored inside its venv in front of that gate, failing it locally on any
    machine that has provisioned — a false failure on a file no one committed.
    """
    return resolve_spike_crate_root() / "target" / "provisioned"


def provisioning_record_path() -> Path:
    """Where `provision()`'s record is left for `runner.py` to read.

    The record is the single source of the environment a subprocess cell needs.
    Recomputing it in the runner would be a second copy of the same derivation,
    and the two drifting is exactly how the cdylib pin gets lost.
    """
    return provisioned_package_root() / "provisioning-record.json"


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
    provisioning_record_path().write_text(json.dumps(record, indent=2) + "\n")
    sys.stdout.write(json.dumps(record, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
