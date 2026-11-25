# Copyright (c) 2025 Tatolab LLC
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
under `platforms` each machine a wheel of it installs on, left out when its one wheel is
pure, so an app can say a module brings nothing on a machine its wheels leave out, and under
`lowest_runtime_version` the lowest `tatolab-stream` its wheels require, which is the lowest
runtime that can resolve it, so an app can say a module needs a newer runtime before adding it.

What a storefront shows before a module is added, each from what the module project declares:
- `category`, one of `CATALOG_CATEGORIES`, from its `pyproject.toml`'s
  `[tool.tatolab.catalog]` `category`;
- `download_size`, the archive's size in bytes;
- under each node, its `inputs` and `outputs` as the listing reads them, each a `name`, a
  `family` and a `description`, left out where the listing reads none;
- `views`, each folder of its `views/` the archive carries, by its `tatolab_view.json`: its
  `view_type` (`<project uuid>:<folder>`), `title`, `description`, `placement` and the
  `devices` among its declared capabilities (`camera`, `microphone`, `gpu`); a manifest that
  does not name what view contract 1.0 requires fails the catalog;
- `gallery`, the URL of each picture `[tool.tatolab.catalog]` `gallery` names, the cover first,
  published beside the archive under `modules/<distribution>-<version>/gallery/`, left out
  when it names none;
- `release_notes`, the workspace `CHANGELOG.md`'s section for the release the archive is
  built at, in Markdown, left out when it has none;
- `copy_version`, the version `add_dependency` records for an import of the archive, the
  SHA-256 of its files' hashes as canonical JSON, and under `earlier_versions` each earlier
  release's `version` and `copy_version` the catalog listed, so an app names the release an
  import's copy carries.

Stdlib only, beside the uv that makes each listing's venv.
Commands:
  `assemble <workspace root> <wheels directory> <release version> <output root>`
  `write-catalog <output root> <uv> <tatolab-stream wheel> [<catalog it replaces>]`
"""

import email.parser
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
# The project the runtime unit carries and links into every project; never a catalog module.
CORE_PROJECT_DIRECTORY_NAMES = ("tatolab-core-nodes",)
# What a storefront files a module under, in the order it offers them.
CATALOG_CATEGORIES = ("Sources", "Audio", "Effects", "Compositing & output", "Control", "I/O & codecs")
VIEWS_DIRECTORY_NAME = "views"
VIEW_MANIFEST_FILE_NAME = "tatolab_view.json"
# What a view manifest must name to read against view contract 1.0, and the devices among the
# capabilities it may declare.
VIEW_MANIFEST_REQUIRED_FIELDS = ("contract", "title", "entry", "placement")
VIEW_PLACEMENTS = ("area", "node")
VIEW_DEVICE_CAPABILITIES = ("camera", "microphone", "gpu")
GALLERY_DIRECTORY_NAME = "gallery"
GALLERY_PICTURE_SUFFIXES = (".png", ".jpg", ".jpeg", ".webp")
CHANGELOG_FILE_NAME = "CHANGELOG.md"
# What an import of a project leaves out, as the runtime's `add_dependency` copies one.
FOLDERS_AN_IMPORT_SKIPS = ("node_modules", "__pycache__")
# The file that makes a folder a Python virtual environment, which an import leaves out whole.
PYVENV_CONFIG_FILE_NAME = "pyvenv.cfg"
VIEW_CONTRACT = "1.0"
# A gallery picture's file name is its URL's last segment as written, so it needs no encoding.
GALLERY_PICTURE_FILE_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")
FILE_NAMES_AN_IMPORT_SKIPS = (".DS_Store",)
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
THE_RUNTIMES_DISTRIBUTION_NAME = "tatolab-stream"
# A requirement's name and its `>=` or `==` version, bare or in parentheses, before any further
# comma-joined bound or marker.
REQUIREMENT_FLOOR = re.compile(r"^\s*(?P<name>[A-Za-z0-9][A-Za-z0-9._-]*)\s*\(?\s*(?:>=|==)\s*(?P<version>[0-9][0-9A-Za-z.+!-]*)")


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


def lowest_runtime_version_the_wheel_requires(wheel_path):
    """The `tatolab-stream` version the wheel at `wheel_path` requires at the lowest, as its `METADATA` names it."""
    wheel_path = Path(wheel_path)
    with zipfile.ZipFile(wheel_path) as wheel_archive:
        metadata_names = [name for name in wheel_archive.namelist() if re.fullmatch(r"[^/]+\.dist-info/METADATA", name)]
        if len(metadata_names) != 1:
            raise ModuleProjectNotAssembled(f"{wheel_path.name} holds no single `.dist-info/METADATA`")
        metadata = email.parser.Parser().parsestr(wheel_archive.read(metadata_names[0]).decode(), headersonly=True)
    floors = (REQUIREMENT_FLOOR.match(requirement) for requirement in metadata.get_all("Requires-Dist") or [])
    requirements = [
        floor for floor in floors if floor and normalized_distribution_name(floor["name"]) == normalized_distribution_name(THE_RUNTIMES_DISTRIBUTION_NAME)
    ]
    if len(requirements) != 1:
        raise ModuleProjectNotAssembled(
            f"{wheel_path.name} requires {THE_RUNTIMES_DISTRIBUTION_NAME} {'with no `>=` or `==` version' if not requirements else 'more than once'}, "
            "so the lowest runtime it runs on is unknown"
        )
    return requirements[0]["version"]


def lowest_runtime_version_the_wheels_require(wheel_paths):
    """The one lowest `tatolab-stream` version every wheel of a module requires; a module whose wheels differ is refused."""
    versions = {lowest_runtime_version_the_wheel_requires(wheel_path) for wheel_path in wheel_paths}
    if len(versions) != 1:
        raise ModuleProjectNotAssembled(
            f"the wheels {', '.join(Path(wheel_path).name for wheel_path in wheel_paths)} require different lowest "
            f"{THE_RUNTIMES_DISTRIBUTION_NAME} versions: {', '.join(sorted(versions))}"
        )
    return versions.pop()


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
    return sorted(
        project_file.parent
        for project_file in packages_directory.glob(f"*/{PROJECT_FILE_NAME}")
        if project_file.parent.name not in CORE_PROJECT_DIRECTORY_NAMES
    )


def pyproject_of_the_module_in(module_project_directory):
    with open(Path(module_project_directory) / "pyproject.toml", "rb") as pyproject_file:
        return tomllib.load(pyproject_file)


def distribution_name_of_the_module_in(module_project_directory):
    """The distribution the module's own `pyproject.toml` builds."""
    return pyproject_of_the_module_in(module_project_directory)["project"]["name"]


def catalog_declaration_of_the_module_in(module_project_directory):
    """The module's `[tool.tatolab.catalog]`: its `category` and the gallery pictures it names, by path; refused when either is not one the catalog takes."""
    module_project_directory = Path(module_project_directory)
    declared = pyproject_of_the_module_in(module_project_directory).get("tool", {}).get("tatolab", {}).get("catalog", {})
    category = declared.get("category")
    if category not in CATALOG_CATEGORIES:
        raise ModuleProjectNotAssembled(
            f"{module_project_directory / 'pyproject.toml'} names {json.dumps(category)} as its `[tool.tatolab.catalog]` category, "
            f"not one of {', '.join(CATALOG_CATEGORIES)}"
        )
    gallery = declared.get("gallery", [])
    if not isinstance(gallery, list) or not all(isinstance(picture, str) for picture in gallery):
        raise ModuleProjectNotAssembled(f"{module_project_directory / 'pyproject.toml'}'s `[tool.tatolab.catalog]` gallery is not a list of paths")
    gallery_pictures = []
    for picture in gallery:
        picture_path = (module_project_directory / picture).resolve()
        if (
            not picture_path.is_relative_to(module_project_directory.resolve())
            or not picture_path.is_file()
            or picture_path.suffix.lower() not in GALLERY_PICTURE_SUFFIXES
            or not GALLERY_PICTURE_FILE_NAME.fullmatch(picture_path.name)
        ):
            raise ModuleProjectNotAssembled(
                f"{module_project_directory.name}'s gallery names {json.dumps(picture)}, "
                f"which is no {', '.join(GALLERY_PICTURE_SUFFIXES)} picture inside its module project "
                "named with letters, digits, `.`, `_` and `-`"
            )
        gallery_pictures.append(picture_path)
    if len({picture.name for picture in gallery_pictures}) != len(gallery_pictures):
        raise ModuleProjectNotAssembled(f"{module_project_directory.name}'s gallery names two pictures of one file name")
    return category, gallery_pictures


def release_notes_of_the_version(workspace_root, version):
    """The body of the workspace `CHANGELOG.md`'s `## [<version>]` section, or nothing when it has none."""
    changelog = Path(workspace_root) / CHANGELOG_FILE_NAME
    if not changelog.is_file():
        return ""
    section = re.search(
        rf"^## \[{re.escape(version)}\][^\n]*\n(?P<body>.*?)(?=^## \[|\Z)",
        changelog.read_text(),
        flags=re.MULTILINE | re.DOTALL,
    )
    return section["body"].strip() if section else ""


def paths_an_import_carries(relative_paths):
    """Each of `relative_paths`, the files of one project, that `add_dependency` copies when it imports the project, in path order."""
    relative_paths = set(relative_paths)
    virtual_environment_folders = {
        relative_path.rsplit("/", 1)[0] + "/" for relative_path in relative_paths if relative_path.endswith(f"/{PYVENV_CONFIG_FILE_NAME}")
    }

    def is_carried(relative_path):
        *folders, file_name = relative_path.split("/")
        return (
            file_name not in FILE_NAMES_AN_IMPORT_SKIPS
            and not any(folder.startswith(".") or folder in FOLDERS_AN_IMPORT_SKIPS for folder in folders)
            and not any(relative_path.startswith(folder) for folder in virtual_environment_folders)
        )

    return sorted(relative_path for relative_path in relative_paths if is_carried(relative_path))


def files_under_the_folder_an_import_carries(folder):
    """Each file under `folder` an import carries, by its path relative to `folder`, in path order."""
    folder = Path(folder)
    return paths_an_import_carries(path.relative_to(folder).as_posix() for path in folder.rglob("*") if path.is_file())


def copy_version_of(member_bytes_by_name):
    """The version `add_dependency` records for an import of these files: the SHA-256 of each one's SHA-256, by path, as canonical JSON."""
    copied_files = {
        member_name: hashlib.sha256(member_bytes_by_name[member_name]).hexdigest() for member_name in paths_an_import_carries(member_bytes_by_name)
    }
    return hashlib.sha256((json.dumps(copied_files, indent=2, sort_keys=True, ensure_ascii=False) + "\n").encode()).hexdigest()


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


def assemble_one_module_project(module_project_directory, wheels_directory, version, output_root, release_notes=""):
    """Write the module project's archive, its gallery and its sidecars under `output_root/modules/`; answers the archive's path."""
    module_project_directory = Path(module_project_directory)
    distribution_name = distribution_name_of_the_module_in(module_project_directory)
    category, gallery_pictures = catalog_declaration_of_the_module_in(module_project_directory)
    wheel_paths = sorted(Path(wheels_directory).glob(wheel_file_name_prefix_of(distribution_name, version) + "*.whl"))
    if not wheel_paths:
        raise ModuleProjectNotAssembled(f"{distribution_name} has no wheel at {version} in {wheels_directory}")
    lowest_runtime_version = lowest_runtime_version_the_wheels_require(wheel_paths)
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
    views_directory = module_project_directory / VIEWS_DIRECTORY_NAME
    if views_directory.is_dir():
        for relative_path in files_under_the_folder_an_import_carries(views_directory):
            member_bytes_by_name[f"{VIEWS_DIRECTORY_NAME}/{relative_path}"] = (views_directory / relative_path).read_bytes()
    for wheel_path in wheel_paths:
        member_bytes_by_name[f"{ARCHIVE_WHEELS_DIRECTORY_NAME}/{wheel_path.name}"] = wheel_path.read_bytes()

    modules_directory = Path(output_root) / MODULES_DIRECTORY_NAME
    modules_directory.mkdir(parents=True, exist_ok=True)
    archive_stem = f"{normalized_distribution_name(distribution_name)}-{version}"
    archive_file_name = f"{archive_stem}.zip"
    archive_path = modules_directory / archive_file_name
    write_the_archive(archive_path, member_bytes_by_name)
    gallery_urls = []
    for picture_path in gallery_pictures:
        published_picture = modules_directory / archive_stem / GALLERY_DIRECTORY_NAME / picture_path.name
        published_picture.parent.mkdir(parents=True, exist_ok=True)
        published_picture.write_bytes(picture_path.read_bytes())
        gallery_urls.append(f"{GET_TATOLAB_COM_URL}/{published_picture.relative_to(output_root).as_posix()}")
    archive_sha256 = sha256_of_file(archive_path)
    Path(str(archive_path) + ARCHIVE_SHA256_SIDECAR_SUFFIX).write_text(f"{archive_sha256}  {archive_file_name}\n")
    entry_without_its_members = {
        "kind": "package",
        "name": distribution_name,
        "title": project_file["name"],
        "summary": project_file["description"],
        "category": category,
        "version": version,
        "copy_version": copy_version_of(member_bytes_by_name),
        "download_url": f"{GET_TATOLAB_COM_URL}/{MODULES_DIRECTORY_NAME}/{archive_file_name}",
        "download_sha256": archive_sha256,
        "download_size": archive_path.stat().st_size,
        "homepage_url": f"{MODULE_SOURCE_URL_PREFIX}/{module_project_directory.name}",
        "lowest_runtime_version": lowest_runtime_version,
    }
    machines = [machine_of_a_wheel_platform_tag(wheel_path.name) for wheel_path in wheel_paths]
    if None not in machines:
        entry_without_its_members["platforms"] = machines
    if gallery_urls:
        entry_without_its_members["gallery"] = gallery_urls
    if release_notes:
        entry_without_its_members["release_notes"] = release_notes
    Path(str(archive_path) + ARCHIVE_LISTING_SIDECAR_SUFFIX).write_text(json.dumps(entry_without_its_members, indent=2) + "\n")
    return archive_path


def assemble_every_module_project(workspace_root, wheels_directory, version, output_root):
    """Each module project's archive this run's wheels make; a module with none is named and left out."""
    assembled = []
    release_notes = release_notes_of_the_version(workspace_root, version)
    for module_project_directory in every_module_project_directory_in(workspace_root):
        try:
            assembled.append(assemble_one_module_project(module_project_directory, wheels_directory, version, output_root, release_notes))
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


def view_members_of_the_archive(archive):
    """Each view the archive's `views/<folder>/tatolab_view.json` declares, as a catalog entry names it."""
    project = json.loads(archive.read(PROJECT_FILE_NAME))["project"]
    members = []
    for member_name in sorted(archive.namelist()):
        if not re.fullmatch(rf"{VIEWS_DIRECTORY_NAME}/[^/]+/{re.escape(VIEW_MANIFEST_FILE_NAME)}", member_name):
            continue
        manifest = json.loads(archive.read(member_name))
        missing = [field for field in VIEW_MANIFEST_REQUIRED_FIELDS if not isinstance(manifest.get(field), str) or not manifest[field]]
        if missing:
            raise ModuleProjectNotDescribed(f"{member_name} does not read against view contract {VIEW_CONTRACT}: it names no {', '.join(missing)}")
        if manifest["contract"] != VIEW_CONTRACT:
            raise ModuleProjectNotDescribed(f"{member_name} is built for view contract {json.dumps(manifest['contract'])}, not {VIEW_CONTRACT}")
        if manifest["placement"] not in VIEW_PLACEMENTS:
            raise ModuleProjectNotDescribed(f"{member_name}'s placement {json.dumps(manifest['placement'])} is not area or node")
        members.append(
            {
                "view_type": f"{project}:{member_name.split('/')[1]}",
                "title": manifest["title"],
                "description": manifest.get("description") or "",
                "placement": manifest["placement"],
                "devices": [capability for capability in manifest.get("capabilities", []) if capability in VIEW_DEVICE_CAPABILITIES],
            }
        )
    return members


def ports_as_the_catalog_names_them(ports):
    """Each port the listing read, as its `name`, `family` and `description`."""
    return [{"name": port["name"], "family": port.get("family"), "description": port.get("description") or ""} for port in ports]


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
    members = []
    for node_type in source.get("node_types", []):
        member = {
            "name": node_type["type"].split(":", 1)[1],
            "import_path": node_type["type"],
            "summary": first_line_of(node_type.get("description")),
        }
        for direction in ("inputs", "outputs"):
            if node_type.get(direction) is not None:
                member[direction] = ports_as_the_catalog_names_them(node_type[direction])
        members.append(member)
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
        entry["views"] = view_members_of_the_archive(archive)
    return entry


def earlier_versions_beside(entry, the_entry_it_replaces):
    """Each earlier release's `version` and `copy_version` the catalog listed for `entry`'s module, oldest first."""
    if the_entry_it_replaces is None:
        return []
    earlier = [*the_entry_it_replaces.get("earlier_versions", [])]
    if the_entry_it_replaces.get("copy_version") and the_entry_it_replaces.get("version") != entry.get("version"):
        earlier.append({"version": the_entry_it_replaces["version"], "copy_version": the_entry_it_replaces["copy_version"]})
    return [earlier_version for earlier_version in earlier if earlier_version.get("version") != entry.get("version")]


def catalog_with_these_entries(entries, catalog_it_replaces):
    """The catalog listing `entries` in name order, beside each entry of `catalog_it_replaces` no entry here names."""
    entries_it_replaces = {
        normalized_distribution_name(entry.get("name", "")): entry for entry in (catalog_it_replaces or {}).get("entries", [])
    }
    named_now = {normalized_distribution_name(entry["name"]) for entry in entries}
    kept = [entry for name, entry in entries_it_replaces.items() if name not in named_now]
    written = []
    for entry in entries:
        earlier_versions = earlier_versions_beside(entry, entries_it_replaces.get(normalized_distribution_name(entry["name"])))
        written.append({**entry, "earlier_versions": earlier_versions} if earlier_versions else entry)
    return {
        "catalog_version": CATALOG_VERSION,
        "entries": sorted([*written, *kept], key=lambda entry: (entry.get("kind") != "package", entry["name"])),
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
