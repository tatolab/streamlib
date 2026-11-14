# Container support

There is no StreamLib image here. The one that used to live in this repo built an
image-local package-source tree, linked the engine into it, served the
Deno SDK over an HTTP mount, and let the runtime compile the api-server from source on
first boot — every mechanism of which the importable-Python-library pivot deletes. It was
removed rather than rewritten; nothing in CI built it. Commit `73078185` is the last one that
carried it (`git show 73078185:Dockerfile`), and the parts of it still worth reading, the
CUDA runtime base, the package set and the PipeWire startup order, are captured in the
learning linked below.

What remains are the two pieces that are about the *host and the hardware*, not about
packaging, and that survive the pivot unchanged:

| | |
|---|---|
| [`../scripts/docker/host-prereqs.sh`](../scripts/docker/host-prereqs.sh) | NVIDIA driver check, Container Toolkit wiring, optional `v4l2loopback` virtual camera nodes. A Dockerfile cannot bake these in — they touch the kernel and the Docker daemon. |
| [`pipewire/10-virtual.conf`](pipewire/10-virtual.conf) | Declarative virtual null sink + source, so a container with no `/dev/snd` still has audio devices at startup. |

The image-layer knowledge those two do not carry — the CUDA runtime base, the GLVND/EGL
dispatch set NVIDIA Vulkan needs even headless, the V4L2 and userspace-audio packages, and
the PipeWire startup order — is this:

- **NVIDIA Vulkan headless:** install `libglvnd0 libgl1 libglx0 libegl1 libgles2 libvulkan1 libx11-6 libxext6` in the image. Without the GLVND dispatch set, `vulkaninfo` reports `ERROR_INCOMPATIBLE_DRIVER` while `nvidia-smi` works. Run with `--gpus all` and `NVIDIA_DRIVER_CAPABILITIES=all`; base on `nvidia/cuda:*-runtime-ubuntu24.04` or `ubuntu:24.04`.
- **Audio with no `/dev/snd`:** install `pipewire pipewire-bin pipewire-alsa wireplumber libspa-0.2-modules dbus`, and start in this order: `XDG_RUNTIME_DIR=/run/user/0` (mode 0700), a dbus session bus, `pipewire`, `wireplumber`, then poll `pw-cli info 0` until it answers.

A deployment image is a short Dockerfile written from that: a CUDA runtime base, those
packages, the runtime unit `cargo xtask build-runtime --release` lays out in
`target/tatolab-runtime/` (its `bin/` on `PATH`), the stream's project with its venv synced, and
`tatolab run`.
