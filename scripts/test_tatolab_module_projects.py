# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The archives and catalog `tatolab_module_projects.py` writes: an import of one syncs offline, and the catalog lists what the runtime's listing reads."""

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
from tatolab.stream import node


@node.meta(description="Makes frames out of nothing.\\n\\nA second paragraph.")
@node.process
class FrameMaker:
    def process(self, ctx):
        pass
'''


def write_a_module_wheel(directory, distribution_name, platform_tag, entry_points):
    """A wheel of `distribution_name` at the release's version holding one node module, declaring `entry_points` under `tatolab.nodes`."""
    underscored_name = distribution_name.replace("-", "_")
    package_name = distribution_name.split("-", 1)[1]
    wheel_path = Path(directory) / f"{underscored_name}-{RELEASE_VERSION}-cp312-abi3-{platform_tag}.whl"
    dist_info = f"{underscored_name}-{RELEASE_VERSION}.dist-info"
    with zipfile.ZipFile(wheel_path, "w") as wheel_archive:
        wheel_archive.writestr(f"tatolab/{package_name}/__init__.py", NODE_MODULE_SOURCE)
        wheel_archive.writestr(
            f"{dist_info}/METADATA",
            f"Metadata-Version: 2.1\nName: {distribution_name}\nVersion: {RELEASE_VERSION}\nRequires-Python: >=3.12\n",
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


def write_a_module_package(workspace_root, directory_name, distribution_name, title):
    """`packages/<directory_name>/` as a module project: a pyproject naming the distribution, a project file, a README and one stream."""
    module_directory = Path(workspace_root) / "packages" / directory_name
    (module_directory / "streams").mkdir(parents=True)
    (module_directory / "pyproject.toml").write_text(f'[project]\nname = "{distribution_name}"\nversion = "0.0.0"\n')
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

    def assemble_the_camera(self, entry_points=None):
        write_a_module_package(self.workspace_root, "tatolab-camera", "tatolab-camera", "Camera")
        for platform_tag in (LINUX_X86_64_PLATFORM_TAG, MACOS_ARM64_PLATFORM_TAG):
            write_a_module_wheel(
                self.wheels_directory,
                "tatolab-camera",
                platform_tag,
                {"FrameMaker": "tatolab.camera:FrameMaker"} if entry_points is None else entry_points,
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
        [archive_path] = self.assemble_the_camera()
        importing_project = self.working_directory / "show"
        with zipfile.ZipFile(archive_path) as archive:
            archive.extractall(importing_project / "imports" / "camera")
        (importing_project / "pyproject.toml").write_text(
            '[project]\nname = "show"\nversion = "0.1.0"\nrequires-python = ">=3.12"\n'
            'dependencies = ["tatolab-camera-module-project"]\n\n'
            '[tool.uv.sources]\ntatolab-camera-module-project = { path = "imports/camera", package = false }\n'
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
        self.assertEqual(entry["nodes"], [{"name": "FrameMaker", "import_path": "tatolab.camera:FrameMaker", "summary": "Makes frames out of nothing."}])
        self.assertEqual(
            entry["platforms"],
            [{"sys_platform": "darwin", "platform_machine": "arm64"}, {"sys_platform": "linux", "platform_machine": "x86_64"}],
        )
        self.assertEqual(entry["streams"], [{"name": "Loop", "import_path": "1f9d2c3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f", "summary": ""}])

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


class CatalogMerge(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
