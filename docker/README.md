# StreamLib container distribution

A multi-stage, GPU-capable Docker image that ships StreamLib as a
**self-contained, headless** runtime service: real NVIDIA Vulkan with no
display server, userspace audio with no hardware, and an **in-container Gitea
package registry pre-filled to match the source checkout** so the runtime can
resolve and build new packages against the registry-by-version model entirely
offline.

This replaces the earlier `.deb` plan — the image *is* the distribution unit,
and the same registry-only resolution model that runs locally
([`docs/architecture/gitea-registry-distribution.md`](../docs/architecture/gitea-registry-distribution.md))
runs inside the container.

## The setup splits across three layers — only one is the Dockerfile

A container image captures **in-image** state only. Kernel modules and device
access live on the host and in the run config.

| Layer | What | Where |
|---|---|---|
| **1. Host** | NVIDIA driver, `nvidia-container-toolkit` wired into Docker, optional `v4l2loopback` virtual camera nodes | [`scripts/docker/host-prereqs.sh`](../scripts/docker/host-prereqs.sh) |
| **2. Image** | Vulkan/GLVND + V4L2 + userspace audio + build toolchain + the filled Gitea + the runtime | [`Dockerfile`](../Dockerfile) |
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

The runtime's HTTP/WebSocket control plane is then on `localhost:9000`; the
in-container Gitea registry is on `localhost:3300` (inside the container).

## What's inside

- **GPU — real, headless.** NVIDIA Vulkan enumerates with no X server because
  the image ships the GLVND/EGL dispatch layer `libGLX_nvidia` sits behind (the
  non-obvious piece — see
  [`docs/learnings/headless-nvidia-vulkan-container.md`](../docs/learnings/headless-nvidia-vulkan-container.md)).
  The display processor degrades to drain-and-drop when no surface is present.
- **Audio — userspace, no hardware.** cpal → ALSA → PipeWire via `pipewire-alsa`;
  a declarative virtual null sink ([`docker/pipewire/10-virtual.conf`](pipewire/10-virtual.conf))
  comes up in the entrypoint. No `/dev/snd`.
- **Registry — in-container, pre-filled.** Stage 1 stands up an ephemeral Gitea,
  publishes the vulkanalia fork + the streamlib crate/python/deno closure + every
  `packages/*` as a `.slpkg`, and bakes the filled data dir into the image. The
  entrypoint serves it (as the `git` user — Gitea refuses root) so
  `runtime.add_module` of a new package resolves in-container.
- **Boot — compiler-free.** The `api-server` core module is pre-materialized into
  the package cache in stage 1, so the runtime boots without rebuilding it. The
  toolchain is present only for *new* runtime module builds.

## Build args

| Arg | Default | Purpose |
|---|---|---|
| `CUDA_BASE` | `nvidia/cuda:13.2.1-runtime-ubuntu24.04` | final base image |
| `RUST_CHANNEL` | `stable` | rustup toolchain for the builder |
| `PREBUILD_API_SERVER` | `1` | pre-materialize api-server (set `0` for a faster build; first boot then builds it) |
| `SKIP_PYTHON_SDK` / `SKIP_DENO_SDK` / `SKIP_PACKAGES` | `0` | skip closure streams during iteration |

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
  supervisor (Gitea + audio backgrounded with readiness polling, runtime exec'd
  as PID 1).
