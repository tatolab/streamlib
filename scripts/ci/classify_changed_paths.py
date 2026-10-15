# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decide which CI lanes a change needs, from the paths it touches.

`ci.yml`'s first job runs this and every heavy job reads one of its outputs.
Classification is by exclusion: a path no rule names runs every lane, so a new
directory, a new root file or a mis-spelt rule costs CI time, never coverage.
A push to `main`, a merge-queue group and a manual dispatch run every lane.
"""

from __future__ import annotations

import argparse
import os
import sys
from dataclasses import dataclass, field
from pathlib import PurePosixPath

LANE_NAMES = (
    "rust",
    "runtime_unit",
    "python_wheel",
    "integration",
    "extension_webrtc",
    "stream_package",
    "python_types",
    "installer",
)

EVENTS_THAT_RUN_EVERY_LANE = frozenset({"push", "merge_group", "workflow_dispatch", "schedule"})

# The lanes that run on the runtime unit; any of them needs it built or restored.
LANES_THAT_RUN_ON_THE_RUNTIME_UNIT = ("python_wheel", "integration", "extension_webrtc")

# Read by no lane: prose, records, and trees CI never builds — the held
# pre-pivot consumers, the examples and the rig's container recipes.
PATH_PREFIXES_NO_LANE_READS = (
    "docs/",
    ".claude/",
    ".github/ISSUE_TEMPLATE/",
    "LICENSES/",
    "examples/",
    "docker/",
    "packages/clap/",
    "packages/jpeg/",
    "packages/screen-capture/",
    "sdk/tatolab-pypi-placeholder/",
)
ROOT_FILES_NO_LANE_READS = frozenset(
    {
        "LICENSE",
        ".github/CODEOWNERS",
        ".github/workflows/pr-title.yml",
        "skills-lock.json",
        ".gitignore",
    }
)
# Markdown outside the trees a build reads. `THIRD-PARTY-NOTICES.md` is not
# among them: both wheels ship it through their `license-files`.
MARKDOWN_IS_PROSE_UNDER = ("docs/", ".claude/", ".github/")
MARKDOWN_SHIPPED_IN_A_DISTRIBUTION = frozenset({"THIRD-PARTY-NOTICES.md"})

# The distribution workflows run on `main` and at release, never on a pull
# request; repo gates lint every workflow file.
DISTRIBUTION_WORKFLOW_PATHS = frozenset(
    {
        ".github/workflows/release.yml",
        ".github/workflows/distribution-dry-run.yml",
        ".github/workflows/reusable-distributions.yml",
        ".github/workflows/reusable-extension-wheel.yml",
        ".github/workflows/reusable-linux-runtime-tarball.yml",
        ".github/workflows/reusable-macos-runtime-unit.yml",
        ".github/workflows/control-plane-client.yml",
    }
)

RELEASE_CONFIGURATION_PATHS = frozenset(
    {
        "release-please-config.json",
        ".release-please-manifest.json",
        "scripts/test_release_configuration.py",
    }
)

STREAM_PACKAGE_PREFIX = "sdk/tatolab-stream/"
# `tatolab new` compiles these into the `tatolab` binary with `include_str!`.
STREAM_PACKAGE_SCAFFOLD_PREFIX = "sdk/tatolab-stream/scaffold_template/"
PYTHON_WHEEL_PREFIX = "sdk/streamlib-python-wheel/"
INTEGRATION_SUITE_PREFIX = "tests/stream-on-runtime/"
WEBRTC_EXTENSION_PREFIX = "packages/streamlib-webrtc/"
INSTALLER_PREFIX = "installer/"


@dataclass
class LaneSelection:
    lanes: dict[str, bool] = field(default_factory=lambda: dict.fromkeys(LANE_NAMES, False))
    reasons: dict[str, list[str]] = field(default_factory=dict)

    def select(self, lane_names: tuple[str, ...], path: str) -> None:
        for lane_name in lane_names:
            self.lanes[lane_name] = True
            self.reasons.setdefault(lane_name, []).append(path)

    def close_over_the_runtime_unit(self) -> None:
        if any(self.lanes[lane_name] for lane_name in LANES_THAT_RUN_ON_THE_RUNTIME_UNIT):
            self.lanes["runtime_unit"] = True


EVERY_CODE_LANE = (
    "rust",
    "runtime_unit",
    "python_wheel",
    "integration",
    "extension_webrtc",
    "stream_package",
    "python_types",
)


def is_a_rust_build_input_in_the_python_wheel(path: str) -> bool:
    relative = path[len(PYTHON_WHEEL_PREFIX) :]
    return (
        relative.startswith("src/")
        or relative in {"Cargo.toml", "build.rs"}
        or relative.endswith(".rs")
    )


def lanes_for_path(path: str) -> tuple[str, ...]:
    """The lanes one changed path needs; every code lane when no rule names it."""
    if path in ROOT_FILES_NO_LANE_READS or path.startswith(PATH_PREFIXES_NO_LANE_READS):
        return ()
    if path.endswith(".md") and path not in MARKDOWN_SHIPPED_IN_A_DISTRIBUTION:
        if "/" not in path or path.startswith(MARKDOWN_IS_PROSE_UNDER):
            return ()
    if path in DISTRIBUTION_WORKFLOW_PATHS:
        return ()
    if path in RELEASE_CONFIGURATION_PATHS:
        return ("python_types",)
    if path.startswith(INSTALLER_PREFIX):
        return ("installer",)
    if path.startswith(INTEGRATION_SUITE_PREFIX):
        return ("integration",)
    if path.startswith(WEBRTC_EXTENSION_PREFIX):
        return ("extension_webrtc",)
    if path.startswith(STREAM_PACKAGE_PREFIX) and not path.startswith(STREAM_PACKAGE_SCAFFOLD_PREFIX):
        return ("stream_package", "python_types", "python_wheel", "integration", "extension_webrtc")
    if path.startswith(PYTHON_WHEEL_PREFIX) and not is_a_rust_build_input_in_the_python_wheel(path):
        return ("python_wheel", "python_types", "integration", "extension_webrtc")
    if path.startswith(".github/"):
        return EVERY_CODE_LANE + ("installer",)
    return EVERY_CODE_LANE


def select_lanes(event_name: str, changed_paths: list[str]) -> LaneSelection:
    selection = LaneSelection()
    if event_name in EVENTS_THAT_RUN_EVERY_LANE:
        selection.select(LANE_NAMES, f"<{event_name} runs every lane>")
        return selection
    for path in changed_paths:
        path = PurePosixPath(path.strip()).as_posix()
        if path in ("", "."):
            continue
        selection.select(lanes_for_path(path), path)
    selection.close_over_the_runtime_unit()
    return selection


def render_github_outputs(selection: LaneSelection) -> str:
    return "".join(f"{lane_name}={'true' if selected else 'false'}\n" for lane_name, selected in selection.lanes.items())


def render_step_summary(selection: LaneSelection) -> str:
    lines = ["### Lanes this change runs", ""]
    for lane_name, selected in selection.lanes.items():
        if not selected:
            lines.append(f"- `{lane_name}`: skipped")
            continue
        reasons = selection.reasons.get(lane_name, ["needed by a lane that runs on the runtime unit"])
        shown = ", ".join(f"`{reason}`" for reason in reasons[:5])
        more = f" and {len(reasons) - 5} more" if len(reasons) > 5 else ""
        lines.append(f"- `{lane_name}`: runs ({shown}{more})")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-name", required=True)
    parser.add_argument(
        "--changed-paths-file",
        help="One repository-relative path per line; omitted when the event runs every lane",
    )
    arguments = parser.parse_args(argv)

    changed_paths: list[str] = []
    if arguments.event_name not in EVENTS_THAT_RUN_EVERY_LANE:
        if not arguments.changed_paths_file:
            parser.error(f"a {arguments.event_name} event needs --changed-paths-file")
        with open(arguments.changed_paths_file, encoding="utf-8") as changed_paths_file:
            changed_paths = changed_paths_file.read().splitlines()

    selection = select_lanes(arguments.event_name, changed_paths)
    rendered_outputs = render_github_outputs(selection)
    sys.stdout.write(rendered_outputs)

    github_output_path = os.environ.get("GITHUB_OUTPUT")
    if github_output_path:
        with open(github_output_path, "a", encoding="utf-8") as github_output:
            github_output.write(rendered_outputs)
    step_summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if step_summary_path:
        with open(step_summary_path, "a", encoding="utf-8") as step_summary:
            step_summary.write(render_step_summary(selection))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
