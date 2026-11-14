# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Tatolab's module projects and the catalog that lists them, laid out as get.tatolab.com serves them.

A module project is a first-party module in `packages/<directory>/` that holds a
`tatolab-project.json`. Its archive, `modules/<distribution>-<version>.zip`, is a project an
import copies whole into `imports/<folder>/`: the project file, the module's README and
licence files, its top-level streams, and every wheel the release built for it under `wheels/`, which its
`pyproject.toml` declares as uv path sources, one per platform, so a project importing it
syncs with no Rust toolchain and no index serving the module. The catalog,
`catalog/v1.json`, lists each archive with its SHA-256, the `@node`s and `@stream`s it
brings, read by the `tatolab.stream` listing the runtime runs, never by parsing source here,
and under `platforms` each machine a wheel of it installs on, left out when its one wheel is
pure, so an app can say a module brings nothing on a machine its wheels leave out.

Stdlib only, beside the uv that makes each listing's venv.
Commands:
  `assemble <workspace root> <wheels directory> <release version> <output root>`
  `write-catalog <output root> <uv> <tatolab-stream wheel> [<catalog it replaces>]`
"""

import hashlib
import json
import re
import subprocess
import sys
import tempfile
import tomllib
import zipfile
from pathlib import Path

PROJECT_FILE_NAME = "tatolab-project.json"
PACKAGES_DIRECTORY_NAME = "packages"
MODULES_DIRECTORY_NAME = "modules"
CATALOG_RELATIVE_PATH = Path("catalog") / "v1.json"
CATALOG_VERSION = 1
ARCHIVE_WHEELS_DIRECTORY_NAME = "wheels"
STREAMS_DIRECTORY_NAME = "streams"
STREAM_DOCUMENT_SUFFIX = ".graph.json"
ARCHIVE_SHA256_SIDECAR_SUFFIX = ".sha256"
ARCHIVE_LISTING_SIDECAR_SUFFIX = ".entry.json"
FILES_A_MODULE_PROJECT_CARRIES_BESIDE_ITS_PROJECT_FILE = ("README.md", "LICENSE", "THIRD-PARTY-NOTICES.md")
GET_TATOLAB_COM_URL = "https://get.tatolab.com"
MODULE_SOURCE_URL_PREFIX = "https://github.com/tatolab/streamlib/tree/main/packages"
PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE = "tatolab.stream._project_node_type_listing_entry"
INSTALLED_DISTRIBUTION_SOURCE = "installed_distribution"
# Every archive member's timestamp, so an archive of the same files is the same bytes.
ARCHIVE_MEMBER_TIMESTAMP = (2025, 1, 1, 0, 0, 0)


class ModuleProjectNotAssembled(Exception):
    """A module project whose archive could not be made, in words naming why."""


class ModuleProjectNotDescribed(Exception):
    """A module project the runtime's listing could not describe, in words naming why."""


def normalized_distribution_name(distribution_name):
    """PEP 503's normalized form of a distribution name."""
    return re.sub(r"[-_.]+", "-", distribution_name).lower()


def wheel_file_name_prefix_of(distribution_name, version):
    """What every wheel file name of `distribution_name` at `version` starts with."""
    return f"{re.sub(r'[-.]+', '_', distribution_name).lower()}-{version}-"


def machine_of_a_wheel_platform_tag(wheel_file_name):
    """The machine a wheel's platform tag installs on, as PEP 508's `sys_platform` and `platform_machine`; `None` for `any`."""
    platform_tag = Path(wheel_file_name).stem.split("-")[-1]
    if platform_tag == "any":
        return None
    if re.fullmatch(r"(many|musl)linux_\d+_\d+_x86_64(\.(many|musl)linux_\d+_\d+_x86_64)*", platform_tag):
        return {"sys_platform": "linux", "platform_machine": "x86_64"}
    if re.fullmatch(r"(many|musl)linux_\d+_\d+_aarch64(\.(many|musl)linux_\d+_\d+_aarch64)*", platform_tag):
        return {"sys_platform": "linux", "platform_machine": "aarch64"}
    if re.fullmatch(r"macosx_\d+_\d+_arm64", platform_tag):
        return {"sys_platform": "darwin", "platform_machine": "arm64"}
    raise ModuleProjectNotAssembled(f"{wheel_file_name} carries the platform tag {platform_tag}, which no module project names a machine for")


def uv_marker_of_a_wheel_platform_tag(wheel_file_name):
    """The PEP 508 marker of the machines a wheel's platform tag installs on; `None` for `any`."""
    machine = machine_of_a_wheel_platform_tag(wheel_file_name)
    if machine is None:
        return None
    return f"sys_platform == '{machine['sys_platform']}' and platform_machine == '{machine['platform_machine']}'"


def toml_basic_string(text):
    """`text` as a TOML basic string."""
    return json.dumps(text)


def module_project_pyproject(distribution_name, version, wheel_file_names):
    """The `pyproject.toml` of a module project carrying `wheel_file_names` of `distribution_name`.

    Its own name is never the module's distribution, since it requires that distribution. An
    import of it is a uv path source the runtime writes with `package = false`, so uv installs
    the wheel the machine's platform names and never builds the module project itself; on a
    machine no wheel names, the requirement's marker leaves the module out.
    """
    markers = []
    for wheel_file_name in wheel_file_names:
        marker = uv_marker_of_a_wheel_platform_tag(wheel_file_name)
        if marker is None and len(wheel_file_names) > 1:
            raise ModuleProjectNotAssembled(f"{distribution_name} has a pure wheel beside platform wheels: {', '.join(wheel_file_names)}")
        markers.append(marker)
    requirement = f"{distribution_name}=={version}"
    if markers != [None]:
        requirement += "; " + " or ".join(f"({marker})" for marker in markers)
    source_lines = []
    for wheel_file_name, marker in zip(wheel_file_names, markers):
        wheel_path = toml_basic_string(f"{ARCHIVE_WHEELS_DIRECTORY_NAME}/{wheel_file_name}")
        if marker is None:
            source_lines.append(f"  {{ path = {wheel_path} }},")
        else:
            source_lines.append(f"  {{ path = {wheel_path}, marker = {toml_basic_string(marker)} }},")
    return "\n".join(
        [
            "[project]",
            f"name = {toml_basic_string(distribution_name + '-module-project')}",
            f"version = {toml_basic_string(version)}",
            'requires-python = ">=3.12"',
            f"dependencies = [{toml_basic_string(requirement)}]",
            "",
            "[tool.uv.sources]",
            f"{distribution_name} = [",
            *source_lines,
            "]",
            "",
        ]
    )


def every_module_project_directory_in(workspace_root):
    """Each `packages/<directory>/` holding a project file, in name order."""
    packages_directory = Path(workspace_root) / PACKAGES_DIRECTORY_NAME
    return sorted(project_file.parent for project_file in packages_directory.glob(f"*/{PROJECT_FILE_NAME}"))


def distribution_name_of_the_module_in(module_project_directory):
    """The distribution the module's own `pyproject.toml` builds."""
    with open(Path(module_project_directory) / "pyproject.toml", "rb") as pyproject_file:
        return tomllib.load(pyproject_file)["project"]["name"]


def sha256_of_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as opened:
        for chunk in iter(lambda: opened.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_the_archive(archive_path, member_bytes_by_name):
    """A `.zip` of `member_bytes_by_name`, its members in name order with one timestamp."""
    with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for member_name in sorted(member_bytes_by_name):
            member = zipfile.ZipInfo(member_name, date_time=ARCHIVE_MEMBER_TIMESTAMP)
            member.compress_type = zipfile.ZIP_DEFLATED
            member.external_attr = 0o644 << 16
            archive.writestr(member, member_bytes_by_name[member_name])


def assemble_one_module_project(module_project_directory, wheels_directory, version, output_root):
    """Write the module project's archive and its sidecars under `output_root/modules/`; answers the archive's path."""
    module_project_directory = Path(module_project_directory)
    distribution_name = distribution_name_of_the_module_in(module_project_directory)
    wheel_paths = sorted(Path(wheels_directory).glob(wheel_file_name_prefix_of(distribution_name, version) + "*.whl"))
    if not wheel_paths:
        raise ModuleProjectNotAssembled(f"{distribution_name} has no wheel at {version} in {wheels_directory}")
    project_file = json.loads((module_project_directory / PROJECT_FILE_NAME).read_text())
    for required_key in ("project", "name", "description"):
        if not project_file.get(required_key):
            raise ModuleProjectNotAssembled(f"{module_project_directory / PROJECT_FILE_NAME} names no `{required_key}`")
    project_file["created_with_runtime"] = version
    member_bytes_by_name = {
        PROJECT_FILE_NAME: (json.dumps(project_file, indent=2, sort_keys=True) + "\n").encode(),
        "pyproject.toml": module_project_pyproject(distribution_name, version, [wheel_path.name for wheel_path in wheel_paths]).encode(),
    }
    for carried_file_name in FILES_A_MODULE_PROJECT_CARRIES_BESIDE_ITS_PROJECT_FILE:
        carried_file = module_project_directory / carried_file_name
        if carried_file.is_file():
            member_bytes_by_name[carried_file_name] = carried_file.read_bytes()
    for stream_document in sorted((module_project_directory / STREAMS_DIRECTORY_NAME).glob(f"*{STREAM_DOCUMENT_SUFFIX}")):
        member_bytes_by_name[f"{STREAMS_DIRECTORY_NAME}/{stream_document.name}"] = stream_document.read_bytes()
    for wheel_path in wheel_paths:
        member_bytes_by_name[f"{ARCHIVE_WHEELS_DIRECTORY_NAME}/{wheel_path.name}"] = wheel_path.read_bytes()

    modules_directory = Path(output_root) / MODULES_DIRECTORY_NAME
    modules_directory.mkdir(parents=True, exist_ok=True)
    archive_file_name = f"{normalized_distribution_name(distribution_name)}-{version}.zip"
    archive_path = modules_directory / archive_file_name
    write_the_archive(archive_path, member_bytes_by_name)
    archive_sha256 = sha256_of_file(archive_path)
    Path(str(archive_path) + ARCHIVE_SHA256_SIDECAR_SUFFIX).write_text(f"{archive_sha256}  {archive_file_name}\n")
    entry_without_its_members = {
        "kind": "package",
        "name": distribution_name,
        "title": project_file["name"],
        "summary": project_file["description"],
        "version": version,
        "download_url": f"{GET_TATOLAB_COM_URL}/{MODULES_DIRECTORY_NAME}/{archive_file_name}",
        "download_sha256": archive_sha256,
        "homepage_url": f"{MODULE_SOURCE_URL_PREFIX}/{module_project_directory.name}",
    }
    machines = [machine_of_a_wheel_platform_tag(wheel_path.name) for wheel_path in wheel_paths]
    if None not in machines:
        entry_without_its_members["platforms"] = machines
    Path(str(archive_path) + ARCHIVE_LISTING_SIDECAR_SUFFIX).write_text(json.dumps(entry_without_its_members, indent=2) + "\n")
    return archive_path


def assemble_every_module_project(workspace_root, wheels_directory, version, output_root):
    """Each module project's archive this run's wheels make; a module with none is named and left out."""
    assembled = []
    for module_project_directory in every_module_project_directory_in(workspace_root):
        try:
            assembled.append(assemble_one_module_project(module_project_directory, wheels_directory, version, output_root))
        except ModuleProjectNotAssembled as not_assembled:
            print(f"::warning::{not_assembled}; its module project is left out of this publish", file=sys.stderr)
    return assembled


def stream_members_of_the_archive(archive):
    """Each stream the archive's `streams/*.graph.json` holds, as a catalog member names it: its name, its asset UUID."""
    members = []
    for member_name in sorted(archive.namelist()):
        if not re.fullmatch(rf"{STREAMS_DIRECTORY_NAME}/[^/]+{re.escape(STREAM_DOCUMENT_SUFFIX)}", member_name):
            continue
        graph_document = json.loads(archive.read(member_name))
        file_stem = Path(member_name).name[: -len(STREAM_DOCUMENT_SUFFIX)]
        members.append({"name": graph_document.get("display_name") or file_stem, "import_path": graph_document["asset"], "summary": ""})
    return members


def first_line_of(description):
    """The first non-blank line of `description`, or nothing."""
    return next((line.strip() for line in (description or "").splitlines() if line.strip()), "")


def node_members_the_listing_names(listing_document, distribution_name):
    """Each node type the listing reads from the installed `distribution_name`, as a catalog member names it."""
    if listing_document.get("failed_because"):
        raise ModuleProjectNotDescribed(f"{distribution_name} was not listed: {listing_document['failed_because']}")
    sources = [
        source
        for source in listing_document.get("sources", [])
        if source.get("source") == INSTALLED_DISTRIBUTION_SOURCE
        and normalized_distribution_name(source.get("distribution", "")) == normalized_distribution_name(distribution_name)
    ]
    if len(sources) != 1:
        raise ModuleProjectNotDescribed(f"{distribution_name} declares no `tatolab.nodes` entry point the listing reads")
    source = sources[0]
    if source.get("unreadable_modules"):
        unreadable = ", ".join(json.dumps(module) for module in source["unreadable_modules"])
        raise ModuleProjectNotDescribed(f"{distribution_name} has modules the listing could not read: {unreadable}")
    undeclared = [node_type["type"] for node_type in source.get("node_types", []) if node_type.get("kind") is None]
    if undeclared:
        raise ModuleProjectNotDescribed(f"{distribution_name} names entry points the listing finds no node at: {', '.join(undeclared)}")
    members = [
        {
            "name": node_type["type"].split(":", 1)[1],
            "import_path": node_type["type"],
            "summary": first_line_of(node_type.get("description")),
        }
        for node_type in source.get("node_types", [])
    ]
    if not members:
        raise ModuleProjectNotDescribed(f"{distribution_name} brings no node the listing reads")
    return members


def run_the_runtime_listing_over_the_archive(archive_path, uv, tatolab_stream_wheel):
    """The listing document `tatolab.stream` prints for a venv holding the archive's Linux x86-64 wheel."""
    with tempfile.TemporaryDirectory() as listing_directory:
        listing_directory = Path(listing_directory)
        with zipfile.ZipFile(archive_path) as archive:
            archive.extractall(listing_directory / "project")
        wheels = [
            wheel
            for wheel in sorted((listing_directory / "project" / ARCHIVE_WHEELS_DIRECTORY_NAME).glob("*.whl"))
            if uv_marker_of_a_wheel_platform_tag(wheel.name) in (None, "sys_platform == 'linux' and platform_machine == 'x86_64'")
        ]
        if not wheels:
            raise ModuleProjectNotDescribed(f"{archive_path.name} carries no wheel the listing's Linux x86-64 venv installs")
        venv = listing_directory / "venv"
        subprocess.run([uv, "venv", "--quiet", "--python", "3.12", str(venv)], check=True)
        python = venv / "bin" / "python"
        subprocess.run([uv, "pip", "install", "--quiet", "--no-deps", "--python", str(python), str(tatolab_stream_wheel), *map(str, wheels)], check=True)
        listed = subprocess.run(
            [str(python), "-I", "-m", PROJECT_NODE_TYPE_LISTING_ENTRY_MODULE],
            cwd=listing_directory / "project",
            input=json.dumps({"dependencies": []}),
            capture_output=True,
            text=True,
            check=False,
        )
        if listed.returncode != 0:
            raise ModuleProjectNotDescribed(f"{archive_path.name}'s listing exited {listed.returncode}: {listed.stderr.strip()}")
        return json.loads(listed.stdout)


def catalog_entry_of_the_archive(archive_path, uv, tatolab_stream_wheel):
    """The archive's catalog entry, its nodes and streams read by the runtime's own listing."""
    archive_path = Path(archive_path)
    entry = json.loads(Path(str(archive_path) + ARCHIVE_LISTING_SIDECAR_SUFFIX).read_text())
    listing_document = run_the_runtime_listing_over_the_archive(archive_path, uv, tatolab_stream_wheel)
    entry["nodes"] = node_members_the_listing_names(listing_document, entry["name"])
    with zipfile.ZipFile(archive_path) as archive:
        entry["streams"] = stream_members_of_the_archive(archive)
    return entry


def catalog_with_these_entries(entries, catalog_it_replaces):
    """The catalog listing `entries` in name order, beside each entry of `catalog_it_replaces` no entry here names."""
    named_now = {normalized_distribution_name(entry["name"]) for entry in entries}
    kept = [
        entry
        for entry in (catalog_it_replaces or {}).get("entries", [])
        if normalized_distribution_name(entry.get("name", "")) not in named_now
    ]
    return {
        "catalog_version": CATALOG_VERSION,
        "entries": sorted([*entries, *kept], key=lambda entry: (entry.get("kind") != "package", entry["name"])),
    }


def write_the_catalog(output_root, uv, tatolab_stream_wheel, catalog_it_replaces_path=None):
    """Write `catalog/v1.json` from every archive under `output_root/modules/`; raises on one that does not describe."""
    output_root = Path(output_root)
    entries = [
        catalog_entry_of_the_archive(archive_path, uv, tatolab_stream_wheel)
        for archive_path in sorted((output_root / MODULES_DIRECTORY_NAME).glob("*.zip"))
    ]
    catalog_it_replaces = None
    if catalog_it_replaces_path is not None and Path(catalog_it_replaces_path).is_file():
        catalog_it_replaces = json.loads(Path(catalog_it_replaces_path).read_text())
    catalog = catalog_with_these_entries(entries, catalog_it_replaces)
    catalog_path = output_root / CATALOG_RELATIVE_PATH
    catalog_path.parent.mkdir(parents=True, exist_ok=True)
    catalog_path.write_text(json.dumps(catalog, indent=2) + "\n")
    return catalog_path


def main(arguments):
    if len(arguments) == 5 and arguments[0] == "assemble":
        assemble_every_module_project(*arguments[1:])
        return 0
    if len(arguments) in (4, 5) and arguments[0] == "write-catalog":
        try:
            write_the_catalog(*arguments[1:])
        except ModuleProjectNotDescribed as not_described:
            print(f"::error::{not_described}", file=sys.stderr)
            return 1
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
