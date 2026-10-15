# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The lane selection `ci.yml` reads: what each kind of change runs and skips."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from classify_changed_paths import LANE_NAMES, select_lanes  # noqa: E402


def lanes_selected_for(*changed_paths: str, event_name: str = "pull_request") -> set[str]:
    selection = select_lanes(event_name, list(changed_paths))
    return {lane_name for lane_name, selected in selection.lanes.items() if selected}


EVERY_CODE_LANE = set(LANE_NAMES) - {"installer"}


class LaneSelectionTests(unittest.TestCase):
    def test_a_docs_only_change_runs_no_lane(self) -> None:
        self.assertEqual(
            lanes_selected_for(
                "docs/plan/ARCHITECTURE.md",
                "README.md",
                "CLAUDE.md",
                ".claude/rules/flow.md",
                ".github/ISSUE_TEMPLATE/bug.yml",
                "LICENSES/Apache-2.0.txt",
            ),
            set(),
        )

    def test_the_held_consumers_and_examples_run_no_lane(self) -> None:
        self.assertEqual(
            lanes_selected_for("examples/camera-display/src/main.rs", "packages/jpeg/src/lib.rs"),
            set(),
        )

    def test_an_engine_change_runs_every_code_lane_and_not_the_installer(self) -> None:
        self.assertEqual(lanes_selected_for("runtime/streamlib-engine/src/lib.rs"), EVERY_CODE_LANE)

    def test_the_lockfile_runs_every_code_lane(self) -> None:
        self.assertEqual(lanes_selected_for("Cargo.lock"), EVERY_CODE_LANE)

    def test_a_path_no_rule_names_runs_every_code_lane(self) -> None:
        self.assertEqual(lanes_selected_for("a-new-top-level-directory/thing.toml"), EVERY_CODE_LANE)

    def test_markdown_inside_a_built_tree_is_not_prose(self) -> None:
        self.assertEqual(lanes_selected_for("runtime/streamlib-engine/README.md"), EVERY_CODE_LANE)

    def test_the_shipped_notices_are_not_prose(self) -> None:
        self.assertEqual(lanes_selected_for("THIRD-PARTY-NOTICES.md"), EVERY_CODE_LANE)

    def test_the_ci_workflow_runs_every_lane(self) -> None:
        self.assertEqual(lanes_selected_for(".github/workflows/ci.yml"), set(LANE_NAMES))
        self.assertEqual(
            lanes_selected_for(".github/actions/install-linux-engine-build-dependencies/action.yml"),
            set(LANE_NAMES),
        )

    def test_a_distribution_workflow_runs_no_lane(self) -> None:
        self.assertEqual(lanes_selected_for(".github/workflows/reusable-distributions.yml"), set())

    def test_an_extension_change_runs_its_lane_on_the_runtime_unit_only(self) -> None:
        self.assertEqual(
            lanes_selected_for("packages/streamlib-webrtc/src/lib.rs"),
            {"extension_webrtc", "runtime_unit"},
        )

    def test_an_integration_suite_change_runs_the_suite_on_the_runtime_unit_only(self) -> None:
        self.assertEqual(
            lanes_selected_for("tests/stream-on-runtime/test_helper_placement.py"),
            {"integration", "runtime_unit"},
        )

    def test_a_stream_package_change_runs_every_python_lane_and_no_rust(self) -> None:
        self.assertEqual(
            lanes_selected_for("sdk/tatolab-stream/tatolab/stream/clock.py"),
            {"stream_package", "python_types", "python_wheel", "integration", "extension_webrtc", "runtime_unit"},
        )

    def test_the_scaffold_template_is_compiled_into_the_cli(self) -> None:
        self.assertEqual(lanes_selected_for("sdk/tatolab-stream/scaffold_template/stream.py"), EVERY_CODE_LANE)

    def test_the_wheels_python_runs_the_python_lanes_and_no_rust(self) -> None:
        self.assertEqual(
            lanes_selected_for("sdk/streamlib-python-wheel/python/tatolab/runtime/_engine.pyi"),
            {"python_wheel", "python_types", "integration", "extension_webrtc", "runtime_unit"},
        )

    def test_the_wheels_rust_runs_every_code_lane(self) -> None:
        self.assertEqual(lanes_selected_for("sdk/streamlib-python-wheel/src/lib.rs"), EVERY_CODE_LANE)
        self.assertEqual(lanes_selected_for("sdk/streamlib-python-wheel/Cargo.toml"), EVERY_CODE_LANE)

    def test_the_installer_runs_its_own_lane_only(self) -> None:
        self.assertEqual(lanes_selected_for("installer/install.sh"), {"installer"})

    def test_release_configuration_runs_the_no_build_python_lane(self) -> None:
        self.assertEqual(lanes_selected_for("release-please-config.json"), {"python_types"})

    def test_lanes_accumulate_across_paths(self) -> None:
        self.assertEqual(
            lanes_selected_for("installer/install.sh", "packages/streamlib-webrtc/pyproject.toml"),
            {"installer", "extension_webrtc", "runtime_unit"},
        )

    def test_a_push_to_main_runs_every_lane(self) -> None:
        self.assertEqual(lanes_selected_for(event_name="push"), set(LANE_NAMES))

    def test_a_merge_group_runs_every_lane(self) -> None:
        self.assertEqual(lanes_selected_for(event_name="merge_group"), set(LANE_NAMES))

    def test_an_empty_change_runs_no_lane(self) -> None:
        self.assertEqual(lanes_selected_for(), set())


if __name__ == "__main__":
    unittest.main()
