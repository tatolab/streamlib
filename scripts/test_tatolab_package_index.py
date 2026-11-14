# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The index tree `tatolab_package_index.py` writes is one uv resolves from."""

import hashlib
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tatolab_package_index  # noqa: E402


def write_minimal_wheel(directory, distribution_name, version):
    """A pure-Python wheel holding one module and the dist-info a wheel needs."""
    underscored_name = distribution_name.replace("-", "_")
    wheel_path = Path(directory) / f"{underscored_name}-{version}-py3-none-any.whl"
    dist_info = f"{underscored_name}-{version}.dist-info"
    with zipfile.ZipFile(wheel_path, "w") as wheel_archive:
        wheel_archive.writestr(f"{underscored_name}.py", "")
        wheel_archive.writestr(
            f"{dist_info}/METADATA",
            f"Metadata-Version: 2.1\nName: {distribution_name}\nVersion: {version}\nRequires-Python: >=3.8\n",
        )
        wheel_archive.writestr(
            f"{dist_info}/WHEEL", "Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
        )
        wheel_archive.writestr(f"{dist_info}/RECORD", "")
    return wheel_path


class TatolabPackageIndexTree(unittest.TestCase):
    def setUp(self):
        self.working_directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.working_directory)
        self.index_root = self.working_directory / "index"
        self.wheel_directory = self.working_directory / "wheels"
        self.wheel_directory.mkdir()

    def test_a_project_page_links_each_wheel_with_its_hash_python_floor_and_metadata_hash(self):
        wheel_path = write_minimal_wheel(self.wheel_directory, "tatolab-testpattern", "0.68.35")
        self.assertTrue(tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path))
        self.assertEqual(tatolab_package_index.write_index_pages(self.index_root), ["tatolab-testpattern"])

        project_page = (self.index_root / "simple/tatolab-testpattern/index.html").read_text(encoding="utf-8")
        wheel_sha256 = hashlib.sha256(wheel_path.read_bytes()).hexdigest()
        metadata_sha256 = hashlib.sha256(
            (self.index_root / f"packages/tatolab-testpattern/{wheel_path.name}.metadata").read_bytes()
        ).hexdigest()
        self.assertIn(f"../../packages/tatolab-testpattern/{wheel_path.name}#sha256={wheel_sha256}", project_page)
        self.assertIn('data-requires-python="&gt;=3.8"', project_page)
        self.assertIn(f'data-core-metadata="sha256={metadata_sha256}"', project_page)
        self.assertIn('href="tatolab-testpattern/"', (self.index_root / "simple/index.html").read_text(encoding="utf-8"))

    def test_a_wheel_held_byte_for_byte_is_left_and_one_held_with_other_bytes_is_refused(self):
        wheel_path = write_minimal_wheel(self.wheel_directory, "tatolab-opus", "0.68.35")
        tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path)
        self.assertFalse(tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path))

        with zipfile.ZipFile(wheel_path, "a") as wheel_archive:
            wheel_archive.writestr("rebuilt.txt", "other bytes")
        with self.assertRaises(tatolab_package_index.WheelAlreadyHeldWithOtherBytes):
            tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path)

    def test_pages_are_written_from_the_sidecars_without_the_wheels(self):
        wheel_path = write_minimal_wheel(self.wheel_directory, "tatolab-h264", "0.68.35")
        tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path)
        (self.index_root / f"packages/tatolab-h264/{wheel_path.name}").unlink()

        tatolab_package_index.write_index_pages(self.index_root)

        self.assertIn(wheel_path.name, (self.index_root / "simple/tatolab-h264/index.html").read_text(encoding="utf-8"))

    @unittest.skipIf(shutil.which("uv") is None, "uv is not installed")
    def test_uv_resolves_a_first_party_module_from_the_tree(self):
        wheel_path = write_minimal_wheel(self.wheel_directory, "tatolab-testpattern", "0.68.35")
        tatolab_package_index.add_wheel_to_index_tree(self.index_root, wheel_path)
        tatolab_package_index.write_index_pages(self.index_root)
        requirements_path = self.working_directory / "requirements.in"
        requirements_path.write_text("tatolab-testpattern==0.68.35\n", encoding="utf-8")

        resolution = subprocess.run(
            [
                "uv", "pip", "compile", "--no-cache", "--no-header", "--python-version", "3.12",
                "--index-url", (self.index_root / "simple").as_uri() + "/", str(requirements_path),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(resolution.returncode, 0, resolution.stderr)
        self.assertIn("tatolab-testpattern==0.68.35", resolution.stdout)


if __name__ == "__main__":
    unittest.main()
