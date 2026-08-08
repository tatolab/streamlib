# Container support

There is no StreamLib image here. The one that used to live in this repo built an
image-local `.slpkg` package-source tree, `streamlib link`ed the engine into it, served the
Deno SDK over an HTTP mount, and let the runtime compile the api-server from source on
first boot — every mechanism of which the importable-Python-library pivot deletes. It was
removed rather than rewritten; nothing in CI built it. Issue #1781 records the commit to
read it at, and which parts of it are still worth reading.

What remains are the two pieces that are about the *host and the hardware*, not about
packaging, and that survive the pivot unchanged:

| | |
|---|---|
| [`../scripts/docker/host-prereqs.sh`](../scripts/docker/host-prereqs.sh) | NVIDIA driver check, Container Toolkit wiring, optional `v4l2loopback` virtual camera nodes. A Dockerfile cannot bake these in — they touch the kernel and the Docker daemon. |
| [`pipewire/10-virtual.conf`](pipewire/10-virtual.conf) | Declarative virtual null sink + source, so a container with no `/dev/snd` still has audio devices at startup. |

The image-layer knowledge those two do not carry — the CUDA runtime base, the GLVND/EGL
dispatch set NVIDIA Vulkan needs even headless, the V4L2 and userspace-audio packages, and
the PipeWire startup order — is in
[`docs/learnings/headless-nvidia-vulkan-container.md`](../docs/learnings/headless-nvidia-vulkan-container.md).
A deployment image for the wheel-hosted runtime is a short Dockerfile written from that
learning: a CUDA runtime base, those packages, `pip install streamlib`, the app, and
`streamlib run`.
