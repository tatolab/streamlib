# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node opens its project asset's bytes by the asset's UUID, from either context."""

import hashlib
import json
import uuid
from pathlib import Path

import pytest

from tatolab.runtime import _engine

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

LUT_BYTES = b"LUT_3D_SIZE 2\n"


def a_project_holding_one_data_asset(project_directory: Path) -> str:
    asset = str(uuid.uuid4())
    content_sha256 = hashlib.sha256(LUT_BYTES).hexdigest()
    (project_directory / "looks").mkdir()
    (project_directory / "looks" / "grade.cube").write_bytes(LUT_BYTES)
    (project_directory / "looks" / "grade.cube.asset.json").write_text(
        json.dumps(
            {
                "asset": asset,
                "kind": "data",
                "content_sha256": content_sha256,
                "content_byte_len": len(LUT_BYTES),
                "import_settings": {},
            }
        )
    )
    return asset


def both_contexts_of_a_node_in(project_directory: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setenv("STREAMLIB_STREAM_PROJECT_DIRECTORY", str(project_directory))
    full_access_context = _engine.open_runtime_context_full_access_for_helper_process(
        {},
        _engine.open_node_link_data_access_for_helper_process(),
        "runtime-under-test",
        "processor-under-test",
    )
    return (
        full_access_context,
        _engine.limited_access_view_of_runtime_context_for_helper_process(full_access_context),
    )


def test_both_contexts_open_the_assets_bytes_by_its_uuid(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    asset = a_project_holding_one_data_asset(tmp_path)

    for context in both_contexts_of_a_node_in(tmp_path, monkeypatch):
        assert Path(context.asset_content_path(asset)).read_bytes() == LUT_BYTES


def test_an_asset_the_project_does_not_hold_is_refused_naming_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    a_project_holding_one_data_asset(tmp_path)
    unknown_asset = str(uuid.uuid4())

    for context in both_contexts_of_a_node_in(tmp_path, monkeypatch):
        with pytest.raises(LookupError, match=unknown_asset):
            context.asset_content_path(unknown_asset)


def test_a_string_that_is_not_a_uuid_is_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    full_access_context, _ = both_contexts_of_a_node_in(tmp_path, monkeypatch)

    with pytest.raises(ValueError, match="not an asset UUID"):
        full_access_context.asset_content_path("looks/grade.cube")


def a_project_importing_one_holding_a_data_asset(project_directory: Path) -> tuple[str, str]:
    imported_project = str(uuid.uuid4())
    asset = str(uuid.uuid4())
    content_sha256 = hashlib.sha256(LUT_BYTES).hexdigest()
    (project_directory / "tatolab-project.json").write_text(
        json.dumps(
            {
                "project": str(uuid.uuid4()),
                "name": "cell",
                "dependencies": [
                    {
                        "project": imported_project,
                        "folder": "robot-arm",
                        "source": "/arm",
                        "version": "0" * 64,
                        "copied_files": {},
                    }
                ],
            }
        )
    )
    import_folder = project_directory / "imports" / "robot-arm"
    (import_folder / "looks").mkdir(parents=True)
    (import_folder / "tatolab-project.json").write_text(
        json.dumps({"project": imported_project, "name": "robot-arm"})
    )
    (import_folder / "looks" / "grade.cube").write_bytes(LUT_BYTES)
    (import_folder / "looks" / "grade.cube.asset.json").write_text(
        json.dumps(
            {
                "asset": asset,
                "kind": "data",
                "content_sha256": content_sha256,
                "content_byte_len": len(LUT_BYTES),
                "import_settings": {},
            }
        )
    )
    return asset, imported_project


def test_both_contexts_open_an_imported_asset_inside_its_import(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    asset, imported_project = a_project_importing_one_holding_a_data_asset(tmp_path)

    for context in both_contexts_of_a_node_in(tmp_path, monkeypatch):
        content_path = context.asset_content_path(asset, from_project=imported_project)
        assert Path(content_path).read_bytes() == LUT_BYTES
        with pytest.raises(LookupError, match=asset):
            context.asset_content_path(asset)
