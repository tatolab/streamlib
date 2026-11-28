# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node interpreter bootstrap's launch describe: the node classes installed
distributions declare in `tatolab.nodes`, and those of the project's own code,
run by path the way the engine runs it at a stream's load."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from test_node_interpreter_bootstrap import (
    DESCRIBED_NODES_SOURCE,
    refusal_of,
    refused_import_paths_in,
    run_the_bootstrap,
    write_project_module,
)


@pytest.fixture
def project_directory(tmp_path: Path) -> Path:
    """A project holding `described_nodes`, whose classes its own code declares."""
    write_project_module(tmp_path, "described_nodes", DESCRIBED_NODES_SOURCE)
    return tmp_path



def install_a_distribution_declaring_nodes(
    project_directory: Path, distribution: str, version: str, entry_points: "dict[str, str]"
) -> None:
    """A `.dist-info` on the project directory, which `importlib.metadata` reads
    as an installed distribution declaring `entry_points` in `tatolab.nodes`."""
    dist_info = project_directory / f"{distribution.replace('-', '_')}-{version}.dist-info"
    dist_info.mkdir()
    (dist_info / "METADATA").write_text(
        f"Metadata-Version: 2.1\nName: {distribution}\nVersion: {version}\n"
    )
    (dist_info / "entry_points.txt").write_text(
        "[tatolab.nodes]\n"
        + "".join(f"{name} = {value}\n" for name, value in entry_points.items())
    )


def installed_modules_of(document: dict[str, Any], distribution: str) -> "list[dict[str, str]]":
    """The installed modules `distribution` declares, by import path — the
    interpreter running the tests may install other `tatolab.nodes` distributions."""
    return sorted(
        (module for module in document["installed_modules"] if module["distribution"] == distribution),
        key=lambda module: module["import_path"],
    )


def test_an_installed_distributions_node_classes_are_described_with_where_they_came_from(
    project_directory: Path,
):
    install_a_distribution_declaring_nodes(
        project_directory,
        "tatolab-ticking",
        "2.4.0",
        {"Ticker": "described_nodes:Ticker", "Missing": "no_such_installed_module:Missing"},
    )

    described = run_the_bootstrap(project_directory, "--describe-installed-modules")

    assert described.returncode == 0, described.stderr
    document = json.loads(described.stdout)
    ours = installed_modules_of(document, "tatolab-ticking")
    our_import_paths = {module["import_path"] for module in ours}
    assert [
        node["import_path"]
        for node in document["described_node_types"]
        if node["import_path"] in our_import_paths
    ] == ["described_nodes:Ticker"]
    assert [
        import_path
        for import_path in refused_import_paths_in(document)
        if import_path in our_import_paths
    ] == ["no_such_installed_module:Missing"]
    assert ours == [
        {
            "import_path": "described_nodes:Ticker",
            "entry_point": "Ticker",
            "distribution": "tatolab-ticking",
            "distribution_version": "2.4.0",
        },
        {
            "import_path": "no_such_installed_module:Missing",
            "entry_point": "Missing",
            "distribution": "tatolab-ticking",
            "distribution_version": "2.4.0",
        },
    ]
    assert "cannot describe no_such_installed_module:Missing, which tatolab-ticking" in (
        described.stderr
    )


def test_an_environment_installing_no_node_classes_lists_no_installed_module(
    project_directory: Path,
):
    described = run_the_bootstrap(project_directory, "--describe-installed-modules")

    assert described.returncode == 0, described.stderr
    document = json.loads(described.stdout)
    assert installed_modules_of(document, "tatolab-ticking") == []
    assert not any(
        node["import_path"] == "no_such_installed_module:Missing"
        for node in document["described_node_types"] + document["refused_node_types"]
    )


def test_the_projects_own_node_classes_are_described_as_its_own(project_directory: Path):
    write_project_module(
        project_directory,
        "broken_nodes",
        """
        from tatolab.stream import EncodedVideo, OutputPin, node

        raise RuntimeError("broken_nodes refuses to import")


        @node.meta(description="never imports")
        @node(execution="continuous", interval_ms=20)
        def Broken(*, frames_to_downstream: OutputPin[EncodedVideo]) -> None: ...
        """,
    )
    imports_directory = project_directory / "imports" / "borrowed"
    imports_directory.mkdir(parents=True)
    write_project_module(imports_directory, "borrowed_nodes", DESCRIBED_NODES_SOURCE)

    described = run_the_bootstrap(project_directory, "--describe-installed-modules")

    assert described.returncode == 0, described.stderr
    document = json.loads(described.stdout)
    project_node_types = [
        "broken_nodes:Broken",
        "described_nodes:LiveCapture",
        "described_nodes:Ticker",
        "described_nodes:WindowedRelay",
    ]
    assert sorted(document["project_node_types"]) == project_node_types
    assert sorted(
        node["import_path"]
        for node in document["described_node_types"]
        if node["import_path"] in project_node_types
    ) == project_node_types[1:]
    assert [
        import_path
        for import_path in refused_import_paths_in(document)
        if import_path in project_node_types
    ] == ["broken_nodes:Broken"]
    assert "broken_nodes refuses to import" in refusal_of(document, "broken_nodes:Broken")
    assert "cannot describe broken_nodes:Broken, which the project's own code declares" in (
        described.stderr
    )


def test_a_project_class_an_installed_distribution_declares_is_described_once_as_installed(
    project_directory: Path,
):
    install_a_distribution_declaring_nodes(
        project_directory, "tatolab-ticking", "2.4.0", {"Ticker": "described_nodes:Ticker"}
    )

    described = run_the_bootstrap(project_directory, "--describe-installed-modules")

    assert described.returncode == 0, described.stderr
    document = json.loads(described.stdout)
    assert "described_nodes:Ticker" not in document["project_node_types"]
    assert [
        node["import_path"]
        for node in document["described_node_types"]
        if node["import_path"] == "described_nodes:Ticker"
    ] == ["described_nodes:Ticker"]
    assert [module["import_path"] for module in installed_modules_of(document, "tatolab-ticking")] == [
        "described_nodes:Ticker"
    ]


def test_describing_the_installed_modules_takes_no_import_paths(project_directory: Path):
    refused = run_the_bootstrap(
        project_directory, "--describe-installed-modules", "described_nodes:Ticker"
    )

    assert refused.returncode == 1
    assert "`--describe-installed-modules` alone" in refused.stderr
