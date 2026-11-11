# offline-new-projects

> **The owner, 2026-11-10**, on #3502: the runtime should always carry every dependency needed
> to create a new project and run it, "at minimum tatolab-stream". On the decision card "How much
> should the runtime carry so a new project works offline?": "Everything, Python too".
> And on this proposal: "Carried first for stream bundling, people should be able to create a new
> project offline with no failures and they should be able to continue working offline (so if
> they've imported and added things to their project it continues working even when offline)".

**The direction, in three sentences:**
1. The runtime unit carries, for the one platform it is built for, a wheel of every third-party
   package its core modules require, pinned by version and SHA-256, and the CPython build its
   pinned uv would download for the scaffold's `.python-version`.
2. Every sync, lock and Python install the runtime runs looks to those first and makes no network
   request, so creating and running a new project works on a machine with no network and an
   empty uv cache.
3. Only a project's own dependencies the unit does not carry reach an index, as today.

This amends the DECIDED create entry in §Product (`ARCHITECTURE.md:344-346`, "uv taking the
Python `.python-version` pins from the machine or fetching it"), and the scope line "Offline create
is not in scope" in `first-party-package-index.md` §What this does not change. It builds on the
tatolab-stream slice of #3502 (#3505).

## Current state (recon, `main` `e91a0df`)

- **The unit carries pure-Python wheels only.** `lib/tatolab/imports/` holds `tatolab-stream` and
  the three core modules (`xtask/src/stage_first_party_modules.rs:20`), declared in every project
  through `.tatolab/first-party-imports/` links
  (`runtime/streamlib-engine/src/core/runtime/project_venv_made_with_the_runtime_unit_uv.rs:204`).
- **What a new project still downloads.** `tatolab-ops` requires `numba>=0.61`, `numpy>=1.26`
  and `pillow>=10.0` (`packages/tatolab-ops/pyproject.toml:21`), and numba requires llvmlite. The
  other core modules and `tatolab-stream` require nothing third-party. The scaffold pins
  `.python-version` to 3.12.
- **Sizes**, cp312 wheels on PyPI (2026-11-10) and the CPython build uv 0.13.1 downloads:

  | | macOS arm64 | Linux x86_64 |
  |---|---|---|
  | llvmlite 0.50.0 | 40.5 MB | 59.7 MB |
  | numpy 2.5.4 | 5.5 MB | 16.7 MB |
  | pillow 13.0.0 | 4.9 MB | 7.1 MB |
  | numba 0.68.1 | 2.8 MB | 3.9 MB |
  | CPython 3.12.15 (`install_only_stripped`, build 20261013) | 25.1 MB | 34.3 MB |
  | **Total** | **about 79 MB** | **about 122 MB** |

- **The runtime's uv calls** are `sync` (`project_venv_made_with_the_runtime_unit_uv.rs:628`) and
  `lock` (`:534`, and `project_first_party_packages_moved_to_the_runtime_version.rs:193`), all
  built from `project_venv_uv_command` (`:634`), which strips the caller's Python and venv
  variables. Nothing passes `--offline`, `--find-links` or a Python mirror today.
- **The unit pins uv by version and SHA-256** (`xtask/src/stage_runtime_unit_uv.rs:17`), and the
  lend's Mesa by tag and SHA-256: the shape every downloaded input of the unit takes.

### Proven by hand (2026-11-10, Linux, uv 0.13.1, network refused, empty `UV_CACHE_DIR` and `UV_PYTHON_INSTALL_DIR`)

- `uv python install 3.12` with `UV_PYTHON_INSTALL_MIRROR=file://<dir>`, where `<dir>` holds
  `20261013/cpython-3.12.15+20261013-x86_64-unknown-linux-gnu-install_only_stripped.tar.gz`,
  installed 3.12.15 in 0.5 s. `uv sync --offline` refuses to install Python even from a `file://`
  mirror ("uv is set to offline mode"), so the install is its own step, before the sync.
- `uv sync --offline --find-links .tatolab/carried-wheels`, a link in the project to the wheel
  folder, resolved and installed `tatolab-stream`, numba, llvmlite, numpy and pillow in 0.6 s.
  `import numba, numpy, PIL, tatolab.stream` ran.
- `uv.lock` records a relative find-links path as written
  (`source = { registry = ".tatolab/carried-wheels" }`), and an absolute one as written. So the
  runtime passes the project's own link, never the unit's path, which differs between machines
  and, inside an AppImage, between runs.

## ADDED

- **The unit's carried wheels.** `cargo xtask build-runtime` downloads, for the platform it
  builds, each wheel `xtask/runtime_unit_carried_packages.toml` pins (name, version, path in
  the unit, URL, SHA-256, per platform), refuses one whose digest differs, and places them in
  `lib/tatolab/carried-wheels/` (`xtask/src/stage_runtime_unit_carried_packages.rs`).
  `cargo xtask pin-runtime-unit-carried-packages` rewrites the pin file: it resolves the core
  modules' third-party requirements with the unit's uv for each platform the unit ships
  (`--python-platform aarch64-apple-darwin` at `MACOSX_DEPLOYMENT_TARGET=26.0`, and
  `x86_64-manylinux_2_28`, Python 3.12), keeps the most specific wheel of each, and writes what
  it resolved. The pin moves only when someone runs it, the way uv's pin does.
- **The unit's carried Python.** The same command pins the CPython build the unit's uv names
  for the scaffold's `.python-version` on each platform (`uv python list --only-downloads`), from
  python-build-standalone's GitHub release, and `build-runtime` places it in
  `lib/tatolab/python-downloads/<build>/<file name>`, the layout uv reads from a mirror.
- **Signed for Apple's notary on macOS.** Apple's notary opens `.whl` and `.tar.gz` archives and
  refuses any Mach-O in them not signed with a Developer ID and a secure timestamp. So
  `build-runtime --macos-developer-id-application-identity <identity>` signs every Mach-O inside
  each carried wheel and the Python archive, with the hardened runtime. The Python's
  `python/bin/` executables also get `xtask/carried_python.entitlements`. Each signed wheel
  member's `RECORD` line takes its new sha256 and size.
  (`xtask/src/sign_the_carried_packages_with_a_developer_id.rs`).
- **The carried Python's sha256, named to uv.** uv checks every Python download against a
  sha256, a mirrored one included, so `build-runtime` writes
  `python-downloads/uv-python-downloads.json`. It names the carried build by uv's key, with the
  sha256 of the archive as placed. The runtime passes it as `UV_PYTHON_DOWNLOADS_JSON_URL` with
  the mirror.
- **A project's link to the carried wheels.** Each time the runtime makes, syncs or locks a
  project's `.venv`, it links `.tatolab/carried-wheels` to the unit's folder, beside
  `.tatolab/first-party-imports/`, and names the link in the project's `pyproject.toml` as
  `[tool.uv].find-links`, so a person's own `uv sync` finds the same wheels. Both are taken out
  when the unit carries none.
- **Carried first, then the index.** When the unit carries a Python and wheels, each sync the
  runtime makes for a project goes in two steps:
  1. `uv python find` for the project's `.python-version`. When it finds none, the runtime runs
     `uv python install --no-bin` with `UV_PYTHON_INSTALL_MIRROR=file://<unit's python-downloads>`,
     which installs the carried one with no network and writes nothing into the person's
     `~/.local/bin`. A version the unit does not carry is left to the sync, which downloads it
     the way it does today.
  2. `uv sync --offline` (and `uv lock --offline` for a lock). If it fails, the runtime runs it
     again without `--offline`. The second run's refusal is the one the action reports, in uv's
     words.
- **Notices.** `THIRD-PARTY-NOTICES.md` names each carried wheel and the CPython build with its
  licence: numpy (BSD-3-Clause, with the libraries its wheel vendors), numba and llvmlite
  (BSD-2-Clause, and LLVM under Apache-2.0 with the LLVM exception), pillow (MIT-CMU, and the
  image libraries its wheel vendors), and CPython (PSF-2.0, with the libraries
  python-build-standalone links). Each wheel and the Python archive keep their own licence files,
  unmodified.
- **A project keeps working offline after create.** What a project holds once it has synced
  stays usable with no network:
  - An imported module project carries its own wheels as path sources (#3216), so it syncs
    offline like the carried ones.
  - A dependency the person adds reaches its index once, when it is added. From then on the
    offline attempt finds it in uv's cache, so a reload, a remade `.venv` or a runtime update
    that resyncs the project needs no network. The runtime never prunes uv's cache. A
    dependency added while offline, and not already in the cache, is refused in uv's words.
- **Proof.** `test_create_project_on_tatolabd.py`, on the unit, on both platforms:
  `create_project` and its first run, with every request refused through an unreachable proxy,
  `UV_CACHE_DIR` and `UV_PYTHON_INSTALL_DIR` empty and uv kept to the Pythons it installs, ends
  `done` with the starter running and the Python installed from the unit. The same test then
  deletes the `.venv` and starts the project again, still offline, and it runs. The engine's own
  tests drive the steps with a stub uv: Python found or installed from the mirror, the offline
  sync, the fall-through to the indexes, the refusal in the second run's words, and a resync once
  the carried wheels change. Adding a dependency online and remaking the `.venv` offline from
  uv's cache was proven by hand with uv 0.13.1.

## MODIFIED

- **§Product, the create entry** (`ARCHITECTURE.md:344-346`): "uv taking the Python
  `.python-version` pins from the machine or fetching it" becomes: uv takes it from the machine,
  or else from the runtime unit, which carries the CPython build for the scaffold's version.
  It fetches only a version the unit does not carry.
- **§Distribution & versioning**: the runtime unit carries, beside the core modules, the
  third-party wheels they require for its platform and one CPython build, pinned by SHA-256. A
  history tag `amended by offline-new-projects` goes on the first entry.
- **`first-party-package-index.md` §What this does not change**: the line "Offline create is not
  in scope" is deleted.
- **`project_venv_uv_command`** gains the mirror variable and the find-links argument, and the
  sync and lock paths gain the offline-then-online attempt. The sync stamp also hashes the carried
  wheels folder's listing, so a unit that carries other wheels resyncs.

## REMOVED

Nothing. This change adds what the unit carries and the order the runtime looks.

## Decisions

1. **DECIDED — Carried first (owner, 2026-11-10).** Every runtime sync and lock tries `--offline`
   against the carried wheels and uv's cache before it reaches an index. A new project never waits
   on the network, and its `uv.lock` names the carried versions. A project that needs a newer
   numpy than the unit carries pins it, and its first sync falls through to the index once.
   Rejected: the index first, carried only when the online sync fails — every sync would wait on
   PyPI again, which #3502 set out to end (owner, 2026-11-10).
2. **Assumed unless overruled: the carried set is the core modules' requirements alone.** The
   scaffold's own `pyproject.toml` requires nothing more. A module project a person imports
   brings its own wheels (#3216), and no other package rides the unit.
3. **Assumed unless overruled: one Python.** The unit carries the one CPython build its uv names
   for the scaffold's `.python-version` (3.12 today). A project that pins another version
   downloads it as today.

## Known risks

- **Notarization of Tato Studio.** Tato Studio is not distributed through the App Store, but
  each release is signed with the Developer ID and notarized for Gatekeeper
  (tatolab-app `release.yml`, `notarize-and-staple.sh`). The app signs and notarizes the unit
  inside its bundle. The carried wheels and the Python archive hold Mach-O binaries signed by their
  builders (numpy's, pillow's, llvmlite's and python-build-standalone's), not with Tatolab's
  Developer ID. Apple's notary service inspects archives in a submission, and may refuse unsigned
  or foreign-signed code inside a `.whl` (a zip). This is not proven either way. The first ticket
  runs a notarization of a bundle holding the carried files before the rest builds on it. If the
  notary refuses, the fallback is to re-sign each Mach-O inside the archives with the app's one
  signing path while the bundle is assembled, which changes the wheels' `RECORD` hashes and needs
  its own decision.
- **The app bundle's allowed entries.** `assemble-tatolab-app-bundle.sh` allows only listed
  `lib/tatolab/*` entries (tatolab/tatolab-app#224). The two new folders need the same allowance,
  filed against tatolab-app with the first ticket.
- **CI's unit cache** grows by the same 79 or 122 MB per platform. The downloads are cached by
  the pin file's digest, so a unit rebuild does not fetch them again.

## Tickets

1. tatolab/tatolab-app#265: the notarization probe, and the bundle's allowed entries.
2. #3502: the pin file, `build-runtime` placing the carried wheels and Python, the runtime's
   two-step sync and lock, the project link, the stamp, and the offline proofs.
3. The notices for the carried wheels and Python, in the same change before a release ships it.
