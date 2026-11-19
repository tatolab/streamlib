# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The archives and catalog `tatolab_module_projects.py` writes: an import of one syncs offline, and the catalog lists what the runtime's listing reads."""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tatolab_module_projects  # noqa: E402

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
RELEASE_VERSION = "0.70.1"
LINUX_X86_64_PLATFORM_TAG = "manylinux_2_28_x86_64"
MACOS_ARM64_PLATFORM_TAG = "macosx_26_0_arm64"

NODE_MODULE_SOURCE = '''
from tatolab.stream import Image, Input, Output, node


@node.meta(description="Makes frames out of nothing.\\n\\nA second paragraph.")
@node.process
class FrameMaker:
    pattern = Input(Image, description="A picture to tile")
    video = Output(Image, description="The frames it makes")

    def process(self, ctx):
        pass
'''

PROGRAM_VIEW_MANIFEST = {
    "contract": "1.0",
    "title": "Program",
    "description": "The show as the audience gets it.",
    "entry": "index.html",
    "placement": "node",
}


def write_a_module_wheel(directory, distribution_name, platform_tag, entry_points, runtime_requirement=f"tatolab-stream>={RELEASE_VERSION}"):
    """A wheel of `distribution_name` at the release's version holding one node module, declaring `entry_points` under `tatolab.nodes` and requiring `runtime_requirement`, none when it is `None`."""
    underscored_name = distribution_name.replace("-", "_")
    package_name = distribution_name.split("-", 1)[1]
    wheel_path = Path(directory) / f"{underscored_name}-{RELEASE_VERSION}-cp312-abi3-{platform_tag}.whl"
    dist_info = f"{underscored_name}-{RELEASE_VERSION}.dist-info"
    with zipfile.ZipFile(wheel_path, "w") as wheel_archive:
        wheel_archive.writestr(f"tatolab/{package_name}/__init__.py", NODE_MODULE_SOURCE)
        wheel_archive.writestr(
            f"{dist_info}/METADATA",
            f"Metadata-Version: 2.1\nName: {distribution_name}\nVersion: {RELEASE_VERSION}\nRequires-Python: >=3.12\n"
            + ("" if runtime_requirement is None else f"Requires-Dist: {runtime_requirement}\n"),
        )
        wheel_archive.writestr(
            f"{dist_info}/WHEEL", f"Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: false\nTag: cp312-abi3-{platform_tag}\n"
        )
        if entry_points:
            wheel_archive.writestr(
                f"{dist_info}/entry_points.txt",
                "[tatolab.nodes]\n" + "".join(f"{name} = {value}\n" for name, value in entry_points.items()),
            )
        wheel_archive.writestr(
            f"{dist_info}/RECORD",
            f"tatolab/{package_name}/__init__.py,,\n{dist_info}/METADATA,,\n{dist_info}/WHEEL,,\n"
            + (f"{dist_info}/entry_points.txt,,\n" if entry_points else "")
            + f"{dist_info}/RECORD,,\n",
        )
    return wheel_path


def write_a_module_package(workspace_root, directory_name, distribution_name, title, catalog_table='category = "Sources"\n'):
    """`packages/<directory_name>/` as a module project: a pyproject naming the distribution and `catalog_table` under `[tool.tatolab.catalog]`, a project file, a README and one stream."""
    module_directory = Path(workspace_root) / "packages" / directory_name
    (module_directory / "streams").mkdir(parents=True)
    (module_directory / "pyproject.toml").write_text(
        f'[project]\nname = "{distribution_name}"\nversion = "0.0.0"\n\n[tool.tatolab.catalog]\n{catalog_table}'
    )
    (module_directory / "tatolab-project.json").write_text(
        json.dumps(
            {"dependencies": [], "description": f"{title} for a stream.", "name": title, "project": "8b0a5a52-3d5d-4f43-8f43-0b6c7b0d1e2f"}
        )
    )
    (module_directory / "README.md").write_text(f"# {title}\n")
    (module_directory / "streams" / "loop.graph.json").write_text(
        json.dumps({"asset": "1f9d2c3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f", "display_name": "Loop", "nodes": [], "links": []})
    )
    return module_directory


class ModuleProjectPyproject(unittest.TestCase):
    def test_each_platform_wheel_is_a_uv_path_source_under_its_platforms_marker(self):
        pyproject = tatolab_module_projects.module_project_pyproject(
            "tatolab-camera",
            RELEASE_VERSION,
            [
                f"tatolab_camera-{RELEASE_VERSION}-cp312-abi3-{LINUX_X86_64_PLATFORM_TAG}.whl",
                f"tatolab_camera-{RELEASE_VERSION}-cp312-abi3-{MACOS_ARM64_PLATFORM_TAG}.whl",
            ],
        )
        read = tomllib.loads(pyproject)
        self.assertEqual(read["project"]["name"], "tatolab-camera-module-project")
        self.assertEqual(
            read["project"]["dependencies"],
            [
                f"tatolab-camera=={RELEASE_VERSION}; (sys_platform == 'linux' and platform_machine == 'x86_64')"
                " or (sys_platform == 'darwin' and platform_machine == 'arm64')"
            ],
        )
        self.assertEqual(
            [source["marker"] for source in read["tool"]["uv"]["sources"]["tatolab-camera"]],
            ["sys_platform == 'linux' and platform_machine == 'x86_64'", "sys_platform == 'darwin' and platform_machine == 'arm64'"],
        )

    def test_a_platform_no_machine_is_named_for_is_refused(self):
        with self.assertRaises(tatolab_module_projects.ModuleProjectNotAssembled):
            tatolab_module_projects.module_project_pyproject("tatolab-camera", RELEASE_VERSION, ["tatolab_camera-0.70.1-cp312-abi3-win_amd64.whl"])


class ModuleProjectArchives(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.uv = shutil.which("uv")
        if cls.uv is None:
            raise unittest.SkipTest("uv is not on PATH")
        cls.tatolab_stream_wheel_directory = Path(tempfile.mkdtemp())
        subprocess.run(
            [cls.uv, "build", "--quiet", "--wheel", "--out-dir", str(cls.tatolab_stream_wheel_directory), str(WORKSPACE_ROOT / "sdk" / "tatolab-stream")],
            check=True,
        )
        cls.tatolab_stream_wheel = next(cls.tatolab_stream_wheel_directory.glob("*.whl"))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tatolab_stream_wheel_directory)

    def setUp(self):
        self.working_directory = Path(tempfile.mkdtemp())
        self.workspace_root = self.working_directory / "workspace"
        self.wheels_directory = self.working_directory / "wheels"
        self.wheels_directory.mkdir()
        self.output_root = self.working_directory / "published"

    def tearDown(self):
        shutil.rmtree(self.working_directory)

    def assemble_the_camera(self, entry_points=None, runtime_requirements=(f"tatolab-stream>={RELEASE_VERSION}",) * 2, before_assembling=None):
        module_directory = write_a_module_package(self.workspace_root, "tatolab-camera", "tatolab-camera", "Camera")
        if before_assembling is not None:
            before_assembling(module_directory)
        for platform_tag, runtime_requirement in zip((LINUX_X86_64_PLATFORM_TAG, MACOS_ARM64_PLATFORM_TAG), runtime_requirements, strict=True):
            write_a_module_wheel(
                self.wheels_directory,
                "tatolab-camera",
                platform_tag,
                {"FrameMaker": "tatolab.camera:FrameMaker"} if entry_points is None else entry_points,
                runtime_requirement,
            )
        return tatolab_module_projects.assemble_every_module_project(self.workspace_root, self.wheels_directory, RELEASE_VERSION, self.output_root)

    def test_an_archive_carries_the_project_its_wheels_and_its_streams_and_names_its_sha256(self):
        [archive_path] = self.assemble_the_camera()
        self.assertEqual(archive_path.name, f"tatolab-camera-{RELEASE_VERSION}.zip")
        with zipfile.ZipFile(archive_path) as archive:
            self.assertEqual(
                sorted(archive.namelist()),
                [
                    "README.md",
                    "pyproject.toml",
                    "streams/loop.graph.json",
                    "tatolab-project.json",
                    f"wheels/tatolab_camera-{RELEASE_VERSION}-cp312-abi3-{MACOS_ARM64_PLATFORM_TAG}.whl",
                    f"wheels/tatolab_camera-{RELEASE_VERSION}-cp312-abi3-{LINUX_X86_64_PLATFORM_TAG}.whl",
                ],
            )
            self.assertEqual(json.loads(archive.read("tatolab-project.json"))["created_with_runtime"], RELEASE_VERSION)
        sidecar_sha256 = Path(str(archive_path) + ".sha256").read_text().split()[0]
        self.assertEqual(sidecar_sha256, tatolab_module_projects.sha256_of_file(archive_path))

    def test_a_module_with_no_wheel_this_run_is_left_out(self):
        write_a_module_package(self.workspace_root, "tatolab-rive", "tatolab-rive", "Rive")
        self.assertEqual(
            tatolab_module_projects.assemble_every_module_project(self.workspace_root, self.wheels_directory, RELEASE_VERSION, self.output_root), []
        )

    def test_an_import_of_the_archive_syncs_offline_installing_the_wheel_its_platform_names(self):
        the_runtimes_version = self.tatolab_stream_wheel.name.split("-")[1]
        [archive_path] = self.assemble_the_camera(runtime_requirements=(f"tatolab-stream>={the_runtimes_version}",) * 2)
        importing_project = self.working_directory / "show"
        with zipfile.ZipFile(archive_path) as archive:
            archive.extractall(importing_project / "imports" / "camera")
        (importing_project / "pyproject.toml").write_text(
            '[project]\nname = "show"\nversion = "0.1.0"\nrequires-python = ">=3.12"\n'
            'dependencies = ["tatolab-camera-module-project", "tatolab-stream"]\n\n'
            '[tool.uv.sources]\ntatolab-camera-module-project = { path = "imports/camera", package = false }\n'
            f'tatolab-stream = {{ path = "{self.tatolab_stream_wheel}" }}\n'
        )
        synced = subprocess.run([self.uv, "sync", "--offline", "--python", "3.12"], cwd=importing_project, capture_output=True, text=True)
        self.assertEqual(synced.returncode, 0, synced.stderr)
        self.assertIn(f"imports/camera/wheels/tatolab_camera-{RELEASE_VERSION}-cp312-abi3-{LINUX_X86_64_PLATFORM_TAG}.whl", (importing_project / "uv.lock").read_text())

    def test_the_catalog_lists_each_archive_with_the_nodes_the_runtime_listing_reads(self):
        self.assemble_the_camera()
        catalog_path = tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel)
        catalog = json.loads(catalog_path.read_text())
        self.assertEqual(catalog["catalog_version"], 1)
        [entry] = catalog["entries"]
        self.assertEqual(entry["kind"], "package")
        self.assertEqual(entry["name"], "tatolab-camera")
        self.assertEqual(entry["title"], "Camera")
        self.assertEqual(entry["download_url"], f"https://get.tatolab.com/modules/tatolab-camera-{RELEASE_VERSION}.zip")
        self.assertEqual(entry["download_sha256"], tatolab_module_projects.sha256_of_file(self.output_root / "modules" / f"tatolab-camera-{RELEASE_VERSION}.zip"))
        self.assertEqual(
            entry["nodes"],
            [
                {
                    "name": "FrameMaker",
                    "import_path": "tatolab.camera:FrameMaker",
                    "summary": "Makes frames out of nothing.",
                    "inputs": [{"name": "pattern", "family": "image", "description": "A picture to tile"}],
                    "outputs": [{"name": "video", "family": "image", "description": "The frames it makes"}],
                }
            ],
        )
        self.assertEqual(entry["category"], "Sources")
        self.assertEqual(entry["download_size"], (self.output_root / "modules" / f"tatolab-camera-{RELEASE_VERSION}.zip").stat().st_size)
        self.assertEqual(entry["views"], [])
        self.assertNotIn("gallery", entry)
        self.assertNotIn("release_notes", entry)
        self.assertEqual(
            entry["platforms"],
            [{"sys_platform": "darwin", "platform_machine": "arm64"}, {"sys_platform": "linux", "platform_machine": "x86_64"}],
        )
        self.assertEqual(entry["streams"], [{"name": "Loop", "import_path": "1f9d2c3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f", "summary": ""}])
        self.assertEqual(entry["lowest_runtime_version"], RELEASE_VERSION)

    def test_an_archive_carries_the_modules_views_and_the_catalog_lists_each(self):
        def write_a_view(module_directory):
            view_directory = module_directory / "views" / "program"
            view_directory.mkdir(parents=True)
            (view_directory / "tatolab_view.json").write_text(json.dumps(PROGRAM_VIEW_MANIFEST))
            (view_directory / "index.html").write_text("<!doctype html>\n")
            (view_directory / "node_modules").mkdir()
            (view_directory / "node_modules" / "left-out.js").write_text("")
            (view_directory / ".DS_Store").write_text("")

        [archive_path] = self.assemble_the_camera(before_assembling=write_a_view)
        with zipfile.ZipFile(archive_path) as archive:
            self.assertEqual(
                sorted(name for name in archive.namelist() if name.startswith("views/")),
                ["views/program/index.html", "views/program/tatolab_view.json"],
            )
        [entry] = json.loads(tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel).read_text())["entries"]
        self.assertEqual(
            entry["views"],
            [{"view_type": "Camera:program", "title": "Program", "description": "The show as the audience gets it.", "placement": "node"}],
        )

    def test_the_catalog_names_the_lowest_runtime_the_modules_wheels_require(self):
        self.assemble_the_camera(runtime_requirements=("tatolab-stream (>=0.69.44)", "tatolab-stream>=0.69.44,<1; python_version >= '3.12'"))
        catalog = json.loads(tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel).read_text())
        [entry] = catalog["entries"]
        self.assertEqual(entry["lowest_runtime_version"], "0.69.44")

    def test_a_module_whose_lowest_runtime_is_unknown_or_differs_between_its_wheels_is_left_out(self):
        for runtime_requirements in ((None, None), ("tatolab-stream", "tatolab-stream"), ("tatolab-stream>=0.69.44", "tatolab-stream>=0.69.45")):
            with self.subTest(runtime_requirements=runtime_requirements):
                shutil.rmtree(self.workspace_root, ignore_errors=True)
                for wheel in self.wheels_directory.glob("*.whl"):
                    wheel.unlink()
                self.assertEqual(self.assemble_the_camera(runtime_requirements=runtime_requirements), [])

    def test_the_catalog_names_only_the_machine_a_linux_only_modules_one_wheel_installs_on(self):
        write_a_module_package(self.workspace_root, "tatolab-browser", "tatolab-browser", "Web page")
        write_a_module_wheel(self.wheels_directory, "tatolab-browser", LINUX_X86_64_PLATFORM_TAG, {"FrameMaker": "tatolab.browser:FrameMaker"})
        tatolab_module_projects.assemble_every_module_project(self.workspace_root, self.wheels_directory, RELEASE_VERSION, self.output_root)
        catalog = json.loads(tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel).read_text())
        [entry] = catalog["entries"]
        self.assertEqual(entry["platforms"], [{"sys_platform": "linux", "platform_machine": "x86_64"}])

    def test_an_archive_whose_module_declares_no_node_fails_the_catalog(self):
        self.assemble_the_camera(entry_points={})
        with self.assertRaises(tatolab_module_projects.ModuleProjectNotDescribed):
            tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel)

    def test_an_entry_point_naming_no_node_fails_the_catalog(self):
        self.assemble_the_camera(entry_points={"Missing": "tatolab.camera:Missing"})
        with self.assertRaises(tatolab_module_projects.ModuleProjectNotDescribed):
            tatolab_module_projects.write_the_catalog(self.output_root, self.uv, self.tatolab_stream_wheel)


class ModuleProjectListingDetails(unittest.TestCase):
    def setUp(self):
        self.working_directory = Path(tempfile.mkdtemp())
        self.workspace_root = self.working_directory / "workspace"
        self.wheels_directory = self.working_directory / "wheels"
        self.wheels_directory.mkdir()
        self.output_root = self.working_directory / "published"

    def tearDown(self):
        shutil.rmtree(self.working_directory)

    def assemble(self, catalog_table='category = "Sources"\n', before_assembling=None):
        module_directory = write_a_module_package(self.workspace_root, "tatolab-camera", "tatolab-camera", "Camera", catalog_table)
        if before_assembling is not None:
            before_assembling(module_directory)
        write_a_module_wheel(self.wheels_directory, "tatolab-camera", LINUX_X86_64_PLATFORM_TAG, {"FrameMaker": "tatolab.camera:FrameMaker"})
        return tatolab_module_projects.assemble_every_module_project(self.workspace_root, self.wheels_directory, RELEASE_VERSION, self.output_root)

    def listing_sidecar_of(self, archive_path):
        return json.loads(Path(str(archive_path) + ".entry.json").read_text())

    def test_a_module_naming_no_category_the_catalog_takes_is_left_out(self):
        for catalog_table in ("", 'category = "Gadgets"\n'):
            with self.subTest(catalog_table=catalog_table):
                shutil.rmtree(self.workspace_root, ignore_errors=True)
                self.assertEqual(self.assemble(catalog_table), [])

    def test_the_gallery_is_published_beside_the_archive_cover_first(self):
        def write_the_pictures(module_directory):
            (module_directory / "catalog").mkdir()
            (module_directory / "catalog" / "cover.png").write_bytes(b"cover")
            (module_directory / "catalog" / "in-use.webp").write_bytes(b"in use")

        [archive_path] = self.assemble('category = "Sources"\ngallery = ["catalog/cover.png", "catalog/in-use.webp"]\n', write_the_pictures)
        gallery_directory = self.output_root / "modules" / f"tatolab-camera-{RELEASE_VERSION}" / "gallery"
        self.assertEqual((gallery_directory / "cover.png").read_bytes(), b"cover")
        self.assertEqual(
            self.listing_sidecar_of(archive_path)["gallery"],
            [
                f"https://get.tatolab.com/modules/tatolab-camera-{RELEASE_VERSION}/gallery/cover.png",
                f"https://get.tatolab.com/modules/tatolab-camera-{RELEASE_VERSION}/gallery/in-use.webp",
            ],
        )

    def test_a_gallery_naming_a_missing_or_outside_or_unpictured_file_is_left_out(self):
        def write_a_text_file(module_directory):
            (module_directory / "notes.txt").write_text("not a picture")

        for gallery in ('["catalog/missing.png"]', '["../outside.png"]', '["notes.txt"]', '"catalog/cover.png"'):
            with self.subTest(gallery=gallery):
                shutil.rmtree(self.workspace_root, ignore_errors=True)
                (self.working_directory / "workspace" / "packages").mkdir(parents=True)
                (self.working_directory / "workspace" / "packages" / "outside.png").write_bytes(b"outside")
                self.assertEqual(self.assemble(f'category = "Sources"\ngallery = {gallery}\n', write_a_text_file), [])

    def test_the_release_notes_are_the_changelogs_section_for_the_release(self):
        self.workspace_root.mkdir(parents=True)
        (self.workspace_root / "CHANGELOG.md").write_text(
            "# Changelog\n\n"
            f"## [{RELEASE_VERSION}](https://example.invalid/compare) (2026-11-19)\n\n\n### Features\n\n* **camera:** sharper frames\n\n"
            "## [0.70.0](https://example.invalid/compare) (2026-11-18)\n\n### Bug Fixes\n\n* older\n"
        )
        [archive_path] = self.assemble()
        self.assertEqual(self.listing_sidecar_of(archive_path)["release_notes"], "### Features\n\n* **camera:** sharper frames")

    def test_the_copy_version_is_the_one_add_dependency_records_for_the_archives_files(self):
        [archive_path] = self.assemble()
        with zipfile.ZipFile(archive_path) as archive:
            copied_files = {name: hashlib.sha256(archive.read(name)).hexdigest() for name in archive.namelist()}
        expected = hashlib.sha256((json.dumps(copied_files, indent=2, sort_keys=True) + "\n").encode()).hexdigest()
        self.assertEqual(self.listing_sidecar_of(archive_path)["copy_version"], expected)

    def test_the_copy_version_follows_the_runtimes_rule_pinned_beside_it(self):
        # The same value `an_imports_version_is_the_sha256_of_its_files_hashes_as_canonical_json`
        # pins in streamlib-project-repository: change both or neither.
        self.assertEqual(
            tatolab_module_projects.copy_version_of({"tatolab-project.json": b"{}\n", "wheels/a.whl": b"wheel", ".tatolab/left-out": b"", "streams/.DS_Store": b""}),
            "e26dca00880b66fe5d7f2f49e2a238ec7c9d881b48793ba29f09dd77d1c397a9",
        )


class CatalogMerge(unittest.TestCase):
    def test_a_new_release_names_each_earlier_releases_copy_version(self):
        catalog = tatolab_module_projects.catalog_with_these_entries(
            [{"kind": "package", "name": "tatolab-camera", "version": "0.70.1", "copy_version": "c3"}],
            {
                "catalog_version": 1,
                "entries": [
                    {
                        "kind": "package",
                        "name": "tatolab-camera",
                        "version": "0.70.0",
                        "copy_version": "c2",
                        "earlier_versions": [{"version": "0.69.9", "copy_version": "c1"}],
                    }
                ],
            },
        )
        [entry] = catalog["entries"]
        self.assertEqual(entry["earlier_versions"], [{"version": "0.69.9", "copy_version": "c1"}, {"version": "0.70.0", "copy_version": "c2"}])

    def test_a_rerun_of_one_release_keeps_its_earlier_versions_and_names_itself_in_none(self):
        catalog = tatolab_module_projects.catalog_with_these_entries(
            [{"kind": "package", "name": "tatolab-camera", "version": "0.70.1", "copy_version": "c3"}],
            {
                "catalog_version": 1,
                "entries": [
                    {
                        "kind": "package",
                        "name": "tatolab-camera",
                        "version": "0.70.1",
                        "copy_version": "c3",
                        "earlier_versions": [{"version": "0.70.0", "copy_version": "c2"}],
                    }
                ],
            },
        )
        [entry] = catalog["entries"]
        self.assertEqual(entry["earlier_versions"], [{"version": "0.70.0", "copy_version": "c2"}])

    def test_an_entry_this_run_did_not_assemble_is_kept_from_the_catalog_it_replaces(self):
        catalog = tatolab_module_projects.catalog_with_these_entries(
            [{"kind": "package", "name": "tatolab-camera", "version": "0.70.1"}],
            {"catalog_version": 1, "entries": [{"kind": "package", "name": "tatolab-camera", "version": "0.70.0"}, {"kind": "package", "name": "tatolab-rive", "version": "0.70.0"}]},
        )
        self.assertEqual(
            catalog["entries"],
            [{"kind": "package", "name": "tatolab-camera", "version": "0.70.1"}, {"kind": "package", "name": "tatolab-rive", "version": "0.70.0"}],
        )


class TheWorkspacesModuleProjects(unittest.TestCase):
    def test_every_module_the_runtime_unit_does_not_carry_is_a_module_project(self):
        distribution_names = sorted(
            tatolab_module_projects.distribution_name_of_the_module_in(directory)
            for directory in tatolab_module_projects.every_module_project_directory_in(WORKSPACE_ROOT)
        )
        self.assertEqual(
            distribution_names,
            [
                "tatolab-browser",
                "tatolab-camera",
                "tatolab-h264",
                "tatolab-h265",
                "tatolab-microphone",
                "tatolab-moq",
                "tatolab-opus",
                "tatolab-rive",
                "tatolab-speaker",
                "tatolab-virtualcamera",
                "tatolab-webrtc",
            ],
        )


    def test_every_module_project_names_a_category_the_catalog_takes(self):
        for directory in tatolab_module_projects.every_module_project_directory_in(WORKSPACE_ROOT):
            with self.subTest(module_project=directory.name):
                category, _ = tatolab_module_projects.catalog_declaration_of_the_module_in(directory)
                self.assertIn(category, tatolab_module_projects.CATALOG_CATEGORIES)


if __name__ == "__main__":
    unittest.main()
