# first-party-package-index

> **Asked for by the owner, 2026-11-07:** "the test pattern is really more representative of the
> old system, we can use it but I don't think it should be a pypi thing since aren't we storing
> modules and other things elsewhere? … a tato studio new and the runtime should probably start
> more or less empty like how blender brings up the editor with a cube … Regarding module
> distribution, would it be better if we moved off using pypi?" Approved 2026-11-08: "Love it,
> let's do it. That also works out well since I imagine for template projects if we ever want to
> host remote we can put all the assets like images, usd files, etc".

**The direction, in four sentences:**
1. Tatolab's first-party modules leave PyPI for Tatolab's own package index, a static PEP 503
   index on the `get-tatolab-com` R2 bucket at `https://get.tatolab.com/simple/`. They stay
   standard wheels that uv resolves, and each project pins `tatolab-*` to that index in its own
   `pyproject.toml`. `tatolab-stream`, the SDK, stays on PyPI, and a third party's module may come
   from any index.
2. Sample projects ship inside the runtime unit at its version, and may later be hosted on the
   same bucket with their assets (images, USD files and so on).
3. A new project starts nearly empty, like Blender's cube: one source, seen in Tato Studio's
   viewer. No effect, no meter, no codec.
4. The test pattern stops being a PyPI dependency of every new project.

This settles the OPEN "Packs, a registry, and loading a stream from a source"
(`ARCHITECTURE.md:879-897`) for first-party distribution, and amends the DECIDED scaffold entry
(`ARCHITECTURE.md:93-116`). It removes the cause of #3172, replaces the PyPI half of #3115, and
reshapes #3216.

## Current state (recon, `main` `8a570a6`)

- **The release uploads every first-party module to PyPI**, one file at a time, because PyPI
  rate-limits new project names: `reusable-distributions.yml:585-698` (`publish-to-pypi`), with
  `pypi_refused_only_new_project_names` letting the Linux runtime publish anyway
  (`reusable-distributions.yml:699-711`). `tatolab-testpattern` and `tatolab-virtualcamera` were
  refused as new names for several releases (#3172), and every project `create_project` wrote then
  failed its first `uv sync`; both are registered now, but the next new name meets the same limit. `tatolab-h264` and `-h265` failed to build for several releases (#3115).
- **The release already writes to the bucket**: `install.sh`, `latest` and the Linux tarball go
  to `get-tatolab-com` through R2's S3 API with a bucket-scoped token
  (`reusable-distributions.yml:778-796`). The index needs no new credential.
- **No first-party module depends on another**: every `packages/*/pyproject.toml` depends on
  `tatolab-stream` by a minimum version, plus `numpy` for `tatolab-browser` and `tatolab-opus`.
  Today no first-party module reaches a project through another first-party module.
- **Every `packages/` name is registered on PyPI** (checked 2026-11-08, `pypi.org/pypi/<name>/json`
  answers 200 for all thirteen); `tatolab-ops`, not yet in `packages/`, is not.
- **The scaffold** depends on `tatolab-stream`, `tatolab-testpattern` and `numpy`
  (`sdk/tatolab-stream/scaffold_template/pyproject.toml:5`), the first two floors raised to the
  runtime's version on render (`runtime/streamlib-engine/src/core/runtime/new_project_scaffold.rs:46-56`,
  `:148-157`). Its one stream asset, `streams/main.graph.json`, is `TestPatternSource` →
  `HalftoneLensEffect` → `BrightnessMeter`, the effect and meter in the template's `nodes/`; the
  encoder and `expose` are already gone (`docs/plan/changes/archive/2026-11-08-gateway-nodes.md:137`).
- **The release checks the scaffold locks this run's wheels** by passing them to `uv lock` as
  find-links beside PyPI (`reusable-distributions.yml:243-300`).
- **Samples have no home**: the scaffold entry sends camera projects to "the docs' sample projects
  and, later, an examples folder the app installs" (`ARCHITECTURE.md:104-106`). #3216 plans to
  list `examples/` in the catalog as `git+https://github.com/tatolab/streamlib@<tag>` requirements.
- **`project-and-assets` is on `main`** (`docs/plan/changes/archive/2026-11-09-project-and-assets.md`): a project is
  a repository of assets with no `stream.py`, and create writes the project file, one stream asset,
  its node modules and the first commit of its history (`project-and-assets.md:160-161`). This
  change sits on top of it: the starter's one stream asset holds one node, and a sample is a
  project of that shape.

## ADDED

- **Tatolab's package index.** A static PEP 503 simple index on `get-tatolab-com`. R2's custom
  domain serves an object at its exact key and maps no directory to an `index.html`, so each page
  is written at the key uv requests, with `Content-Type: text/html`: `simple/` lists every
  project, `simple/<normalized name>/` lists every wheel of that project with its `#sha256=` fragment, `data-requires-python` and
  `data-core-metadata="sha256=<hash>"` (PEP 714's spelling of PEP 658), and each wheel lives at
  `packages/<normalized name>/<wheel file name>` with its `.metadata` file beside it, so uv
  resolves without downloading a wheel. A wheel is written once and never overwritten: a release
  that would write a file the bucket already holds with different bytes fails. The release
  regenerates the index pages from what the bucket holds, after the wheels and before `latest`,
  in one GitHub Actions concurrency group that queues runs with `cancel-in-progress` off, so two releases never
  write the pages at once and a later run's pages always list an earlier run's wheels. Before the
  job succeeds it resolves this run's `tatolab-testpattern` from `https://get.tatolab.com/simple/`
  itself, so the public path, not only the bucket, is proven.
- **The index a project names.** Every project that uses a first-party module names the index in
  its `pyproject.toml`, in the standard uv form, so `uv sync` resolves it with no Tatolab
  machinery:

  ```toml
  [[tool.uv.index]]
  name = "tatolab"
  url = "https://get.tatolab.com/simple/"
  ```

  A named index takes priority over PyPI, and uv's default `first-index` strategy takes a package
  from the first index that has it and never looks further. So every `tatolab-*` module comes from
  ours, a direct dependency or a transitive one through a third party's module alike, and a
  `tatolab-*` name on PyPI never stands in for one our index serves. A name our index does not
  serve falls through to PyPI, which is why every first-party name is also held on PyPI by
  Tatolab (decision 2), a new module's name included, registered when its directory lands in
  `packages/`; every other name our index does not serve (`numpy`,
  `tatolab-stream`, a third party's module) answers 404 there and resolves from PyPI. A per-package
  `explicit = true` pin is not used: uv applies `[tool.uv.sources]` only to a project's own
  dependencies, so a first-party module reached through a third party's would miss it. Every
  first-party requirement the runtime writes is floored at the runtime's version, and a PyPI
  placeholder is version `0.0.x`, so a placeholder never satisfies one: resolution fails naming
  the module instead of syncing an environment without it. A placeholder's one release is yanked
  once uploaded, so a third party's looser requirement fails the same way.
- **Samples in the runtime unit.** The unit carries sample projects at its own version, beside
  the lend as everything else it carries: the Linux tarball under `lib/tatolab/samples/<name>/`,
  the macOS helper bundle under `Contents/Resources/samples/<name>/`.
  A sample is a project directory as a created project is, naming the index. The local API lists them (`list_samples`: name, title, summary per sample) and
  `create_project` takes an optional `sample` name, which copies that sample into the directory
  instead of rendering the starter, then makes the `.venv` and loads it as every create does.
  `tatolab new --sample <name>` is the same call. A sample the unit does not carry is refused by
  name, listing the ones it does.
- **Remote samples, later and unbuilt.** A sample too large for the unit, or one released apart
  from it, is a project archive on the same bucket with its assets beside it, listed in the
  catalog. Nothing here builds it; the catalog's `sample` kind is kept for it.

## MODIFIED

- **First-party distribution** (`ARCHITECTURE.md:3689-3699`, `:569-580`, `:511-517`, `:907-918`).
  The release publishes `tatolab-stream` to PyPI and every first-party module to Tatolab's index,
  at the one version. A module still depends on `tatolab-stream` by a minimum version and on
  nothing of the runtime's. "Published to PyPI beside `tatolab-stream`" reads "published to
  Tatolab's index" in each of those entries.
- **The package systems** (`ARCHITECTURE.md:480`). "PyPI and cargo are the package systems"
  becomes: standard Python package indexes and cargo. A stream's environment resolves from PyPI
  and from any index its `pyproject.toml` names; first-party modules come from Tatolab's.
- **The release order** (`reusable-distributions.yml:1-21`, `:585-711`). `publish-to-pypi`
  uploads `tatolab-stream` alone. A new `publish-to-the-tatolab-index` job uploads every module's
  wheels to the bucket and regenerates the pages. The Linux runtime publishes after both succeed,
  because its scaffold locks against both, and tatolab-app's release, which builds the macOS and
  Linux apps from the tag, starts only once this repository's release has published both, as it
  should wait for PyPI today. Whatever starts it, the app's release first locks the starter
  against the public index and PyPI at the version it bundles, and stops there when either does
  not yet serve it. With one PyPI project and no new names, PyPI's new-project limit no
  longer bites, so no partial-success path remains.
- **The release proof** (`ARCHITECTURE.md:3768-3781`, the macOS proof and `main`'s scaffolded-stream
  run). The scaffolded stream it runs is the starter, so it checks `TestPatternSource.video` and
  nothing else: no effect, no encoder. `graph` counts a port's writes only once something reads
  it (`scripts/check_scaffolded_stream_runs_on_the_driver_the_macos_lend_carries.sh:119-120`), and the
  starter links nothing, so the proof reads the port through `gateway-nodes`' viewer read path
  for the run, as Tato Studio's viewer does, and checks at least sixty frames arrive (decision 5). It changes in
  the same ticket as the scaffold, so a proof never looks for a node the starter lost.
- **The scaffold lock check** (`reusable-distributions.yml:243-300`) points the `tatolab` index at
  a local index built from this run's wheels (uv's command-line `--index tatolab=<file url>`
  overrides the project's index of that name) and keeps `UV_FIND_LINKS` for `tatolab-stream`
  alone. It still fails when the lock takes any version but the one the run built.
- **The scaffold** (`ARCHITECTURE.md:93-116`). The starter is one stream asset holding one source
  node, `TestPatternSource` from `tatolab-testpattern`, which the person sees in Tato Studio's
  viewer. The effect and meter are this change's to remove. The template loses `nodes/`, `HalftoneLensEffect`,
  `BrightnessMeter` and `numpy`; its dependencies are `tatolab-stream` from PyPI and
  `tatolab-testpattern` from the index the project names, both floored at the runtime's version. Ruff, pyright
  and the cross-floor check still gate it. The pathway the old scaffold modelled (pixels on the
  GPU, logic on the CPU) moves to a sample.
- **Where camera projects live** (`ARCHITECTURE.md:104-106`). "The docs' sample projects and,
  later, an examples folder the app installs" becomes the samples the runtime unit carries.
- **Creating a project** (`ARCHITECTURE.md:327-347`). Create renders the starter or copies a
  sample; everything after the files are written is unchanged.
- **The catalog** (#3216, read by tatolab-app's Get more, tatolab-app#133). A `package` entry
  gains `index_url`: `https://get.tatolab.com/simple/` for a first-party module, `null` for one on
  PyPI. Import adds the requirement and, when `index_url` is set and the project does not yet
  name it, the `[[tool.uv.index]]` table. A third party's index is added after `tatolab` with
  `explicit = true` and a `[tool.uv.sources]` pin for the module imported, so it serves only that
  module and never a `tatolab-*` name; a project whose owner orders another index first by hand
  has chosen it. Version 1 lists packages only; Get more's Samples
  scope reads `list_samples` from the runtime, so the samples shown always match the runtime that
  will create them. `sample` entries return with remote samples.
- **The packs OPEN** (`ARCHITECTURE.md:879-897`). Settled: the registry for first-party modules is
  Tatolab's index, and its manager UI is Get more over the catalog. Still open: `run <url-or-zip>`,
  a listing for third parties' modules, and how a package's own streams reach a project, which the
  `project-and-assets` answers with linked dependencies.

## REMOVED

- REMOVED: pypi_refused_only_new_project_names
  The partial-success output and the `publish-linux-runtime` condition that reads it.
- REMOVED: sdk/tatolab-stream/scaffold_template/nodes
  The halftone lens and the brightness meter with it; they live on in the sample that keeps
  the pathway (decision 3).

## Decisions

1. **DECIDED — The starter's one source (owner, 2026-11-08).** `TestPatternSource` from
   `tatolab-testpattern`, resolved from the index: buildable now, it needs no new node, and the
   branded animation is what a person sees first. Once `tatolab-ops` ships with
   `lightweight-node-kinds`, an image generator there can replace it, which changes one template
   line. Rejected: a generator node in the project's own `nodes/`, which needs a zero-input image
   node that `@node.shader` (Image in, Image out) does not give.
2. **Assumed unless overruled: the PyPI copies.** The first-party projects already on PyPI stay
   registered so nobody else takes the names, and a new module's name is registered there with a
   placeholder that installs nothing, as the bare `tatolab` name is. They take no further uploads, and have every release
   yanked once the index serves the same version, so uv and pip skip them unless a project pins
   one exactly. Yanking is reversible. Before it yanks, that ticket checks each name's PyPI owner
   and maintainer list on the project's account page. An existing project gains the `[[tool.uv.index]]` table by
   hand, as the release note says, before its next `uv lock --upgrade`; that note ships in the
   release before the one that yanks; nothing rewrites it on
   load, since pre-1.0 the plan keeps no back-compat shims, and `project-and-assets` already
   changes every project's shape.
3. **Assumed unless overruled: which examples become samples.** Every converted example in
   `examples/` ships as a sample; held pre-pivot ones do not. One of them carries the old
   scaffold's pathway, the halftone lens and the brightness meter.
4. **Assumed unless overruled: the address.** `https://get.tatolab.com/simple/`, the index named
   `tatolab` in every project. A custom domain in front of R2 already serves the bucket, and pages
   written at their directory keys need no Worker.
5. **Assumed unless overruled: the proof's reader.** The scaffold ticket lands after
   `gateway-nodes`' viewer read path, which the proof uses to read the starter's one port. Landing
   first would need a second reader in the proof alone, which nothing else wants.

## Amended — a core rides the runtime, the rest become module projects (owner, 2026-11-10)

> **The owner, 2026-11-10**, on the decision card "Split modules into a runtime core and
> importable module projects?": "Core in runtime, rest as projects". And: "at some point we have
> to accept as a trade off … those devs can also just package that stuff as wheels for their own
> projects … so there are ways we can document people do it".

This extends self-contained-project-imports Decided 10 from `tatolab-ops` to a core set, and
reshapes how every other first-party module reaches a project:

- **The core rides the runtime as wheels.** `cargo xtask build-runtime` builds `tatolab-ops`,
  `tatolab-testpattern` and `tatolab-compositor` (`FIRST_PARTY_PACKAGES_THE_RUNTIME_UNIT_CARRIES_RELATIVE_TO_WORKSPACE`
  in `xtask/src/stage_first_party_modules.rs`) into one wheel each in `lib/tatolab/imports/` with
  `uv build --wheel`. A core module is pure Python, so the unit build compiles nothing more.
- **`tatolab-stream` rides the runtime beside the core** (owner, 2026-11-10, recorded in #3502:
  the runtime carries every package a new project needs, "at minimum tatolab-stream"). The unit
  builds its wheel from the runtime's own commit into the same folder, and a project declares it
  through its link as it declares a core wheel, so a project the runtime makes or syncs never
  waits on PyPI for it. PyPI keeps publishing `tatolab-stream` for use outside a runtime.
- **A project declares each core wheel.** Each time the runtime makes, syncs or locks a
  project's `.venv`, it links each wheel into `.tatolab/first-party-imports/<wheel file name>`
  and declares it as a requirement and a uv path source through that link. The link keeps the
  wheel's file name, since uv reads its tags from it, so a source into the links folder is
  written again whenever the runtime's version changes; a source the project holds outside the
  links folder is kept as written.
- **The starter depends on `tatolab-stream` alone.** Its `TestPatternSource` comes from the
  wheel the runtime declares. It still names Tatolab's index for the modules the runtime does not
  carry, until those are module projects.
- **Every other first-party module becomes a module project**, as self-contained-project-imports
  makes every import: its project file, its streams, its Python package and, for a native module,
  a prebuilt wheel for each platform it runs on, declared as a uv path source inside the project,
  since a machine importing it has no Rust toolchain. The release publishes each as an archive on
  `get.tatolab.com` with its SHA-256, and Get more copies one into `imports/<module>/`. This
  reshapes #3216. A third party ships a native module the same way, prebuilt wheels inside its
  project, and the docs show how.
- **Assumed unless overruled: the index meanwhile.** The release keeps publishing every module to
  `https://get.tatolab.com/simple/` until the module projects ship; the PyPI copies follow #3471.

## What this does not change

- `tatolab-stream` and the Rust crate keep their channels, and the runtime unit still never goes
  through any index.
- Module authoring, naming (`tatolab-<name>` importing as `tatolab.<name>`) and the one version
  are unchanged.
- A third party publishes wherever they like; a project names that index the same way.
