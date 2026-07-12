# StreamLib container distribution

A multi-stage, GPU-capable Docker image that ships StreamLib as a
**self-contained, headless** runtime service: real NVIDIA Vulkan with no
display server, userspace audio with no hardware, and an **image-local static
registry tree pre-filled to match the source checkout** so the runtime can
resolve and build new packages against the registry-by-version model entirely
offline. There is no registry daemon — the tree is plain files, served for
cargo/npm by a dumb `python3 -m http.server` mount and read for pypi/`.slpkg`
straight off `file://`.

This replaces the earlier `.deb` plan — the image *is* the distribution unit,
and the same registry-only resolution model that runs locally
([`docs/architecture/static-registry.md`](../docs/architecture/static-registry.md))
runs inside the container.

## The setup splits across three layers — only one is the Dockerfile

A container image captures **in-image** state only. Kernel modules and device
access live on the host and in the run config.

| Layer | What | Where |
|---|---|---|
| **1. Host** | NVIDIA driver, `nvidia-container-toolkit` wired into Docker, optional `v4l2loopback` virtual camera nodes | [`scripts/docker/host-prereqs.sh`](../scripts/docker/host-prereqs.sh) |
| **2. Image** | Vulkan/GLVND + V4L2 + userspace audio + build toolchain + the static registry tree + the runtime | [`Dockerfile`](../Dockerfile) |
| **3. Run** | `--gpus all`, camera `--device`, RT opt-in | [`docker-compose.yml`](../docker-compose.yml) / `docker run` |

## Quick start

```bash
# 1. Host prerequisites (driver assumed present; wires the toolkit, optional cam node)
INSTALL_TOOLKIT=1 V4L2_NODES="10" scripts/docker/host-prereqs.sh

# 2. Build + run
docker compose up --build
#   …or without compose:
docker build -t streamlib:latest .
docker run --rm --gpus all -e NVIDIA_DRIVER_CAPABILITIES=all -p 9000:9000 streamlib:latest
```

The runtime's HTTP/WebSocket control plane is then on `localhost:9000`. The
static registry mount the entrypoint serves for cargo/npm is localhost-internal
(`127.0.0.1:8799` inside the container) and not exposed.

## What's inside

- **GPU — real, headless.** NVIDIA Vulkan enumerates with no X server because
  the image ships the GLVND/EGL dispatch layer `libGLX_nvidia` sits behind (the
  non-obvious piece — see
  [`docs/learnings/headless-nvidia-vulkan-container.md`](../docs/learnings/headless-nvidia-vulkan-container.md)).
  The display processor degrades to drain-and-drop when no surface is present.
- **Audio — userspace, no hardware.** cpal → ALSA → PipeWire via `pipewire-alsa`;
  a declarative virtual null sink ([`docker/pipewire/10-virtual.conf`](pipewire/10-virtual.conf))
  comes up in the entrypoint. No `/dev/snd`.
- **Registry — image-local static tree, no daemon.** Stage 1 runs
  `cargo xtask static-registry emit --cargo-closure` to write a plain on-disk
  tree at `/opt/streamlib/registry`: a cargo sparse closure + the vulkanalia
  fork, a pypi-simple tree, an npm packument set, the `.slpkg` generic store,
  the catalog, and the release manifest (written last, the atomicity flip). The
  entrypoint serves it for cargo + npm on `127.0.0.1:8799` (sparse + npm are
  HTTP-only by spec) via `python3 -m http.server`; pypi + `.slpkg` read the same
  tree over `file://` with no server. A `[source]` replacement in
  `$CARGO_HOME/config.toml` redirects the canonical `registry.tatolab.com` index
  to that mount so `runtime.add_module` of a package — and any in-tree lib it
  cargo-depends on — resolves in-container while keeping the canonical id in
  every `Cargo.lock`.
- **Boot — builds the core module on first start.** The runtime compiles
  `api-server` from source on first boot against the image-local tree
  (build-capable image, warm cargo cache → tens of seconds); a build-time
  resolution preflight fails the image build fast if a dependency can't resolve.
  The toolchain stays for runtime module builds.

## Build args

| Arg | Default | Purpose |
|---|---|---|
| `CUDA_BASE` | `nvidia/cuda:13.2.1-runtime-ubuntu24.04` | final base image |
| `RUST_CHANNEL` | `stable` | rustup toolchain for the builder |
| `SKIP_PYTHON_SDK` | `0` | skip the pypi (python SDK) tree during iteration (`--no-pypi`) |
| `SKIP_DENO_SDK` | `0` | skip the npm (deno SDK) tree during iteration (`--no-npm`) |
| `SKIP_PACKAGES` | `0` | skip the `.slpkg` store + release manifest during iteration (`--no-slpkg`) |

## Run config

- `--gpus all` + `NVIDIA_DRIVER_CAPABILITIES=all` — required for Vulkan/CUDA.
- Camera: `--device /dev/videoN` (a `v4l2loopback` node), or use a synthetic /
  file source in the graph for a zero-host-dependency container.
- Low-latency audio (drone): `--cap-add SYS_NICE --ulimit rtprio=95` (degrades to
  non-RT without).

## Notes

- Host driver + toolkit reference verified: Ubuntu 24.04, RTX 3090, driver
  595.71.05, NVIDIA Container Toolkit 1.19.x. `nvidia/vulkan` is abandoned — do
  not use it; the CUDA runtime base + GLVND libs is the headless-Vulkan recipe.
- systemd-in-Docker is avoided; [`docker/entrypoint.sh`](entrypoint.sh) is the
  supervisor (registry mount + audio backgrounded with readiness polling,
  runtime exec'd as PID 1).
