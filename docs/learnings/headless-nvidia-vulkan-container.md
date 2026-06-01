# Headless NVIDIA Vulkan + userspace audio in a container

Two independent surprises that bite when running the StreamLib runtime in a
GPU container with no display server and no sound hardware. Both cost real
iteration time to isolate.

## Part 1 — NVIDIA Vulkan needs the GLVND/EGL dispatch layer, even headless

### Symptom

Inside a container started with `--gpus all` (NVIDIA Container Toolkit), with
the NVIDIA ICD JSON and vendor libs mounted and `nvidia-smi` / CUDA working,
Vulkan still reports no device:

```
$ vulkaninfo --summary
... Found no drivers!
... ERROR_INCOMPATIBLE_DRIVER
```

`vkCreateInstance` / `vkEnumeratePhysicalDevices` see zero physical devices.

### Root cause

The NVIDIA Container Toolkit injects only NVIDIA's **vendor** libraries
(`libGLX_nvidia`, `libEGL_nvidia`, the ICD JSON at
`/etc/vulkan/icd.d/nvidia_icd.json`, the driver `.so`s). On a modern Linux
system those vendor libs sit **behind the GLVND/EGL dispatch layer** —
`libGLX_nvidia`'s `vk_icdGetInstanceProcAddr` resolves through GLVND. If the
dispatch layer is absent from the image, that entrypoint returns `NULL`, the
loader concludes the ICD is unusable, and you get `ERROR_INCOMPATIBLE_DRIVER`
— *even though* the ICD JSON is present, the vendor libs are mounted, and CUDA
(which doesn't go through GLVND) works fine. The completeness of the toolkit's
mounts is what makes this misleading: it looks like everything is there.

### Fix

Install the GLVND/EGL dispatch libraries **in the image** (they are userspace,
not driver, so they belong in the image, not the host mount):

```
libglvnd0 libgl1 libglx0 libegl1 libgles2
```

plus the Vulkan loader and the X11 *client* libs `libGLX_nvidia` links against
(needed even with no X *server*):

```
libvulkan1 libx11-6 libxext6
```

With those present, NVIDIA Vulkan enumerates **headless** — no X server, no
`DISPLAY`, no Weston, no `LIBGL_ALWAYS_SOFTWARE`/llvmpipe. The display
processor degrades to drain-and-drop when there's no surface, so a headless
GPU pipeline (camera → compute → drain) runs end to end off `/dev/dri/renderD128`.

### What is NOT the cause (all ruled out empirically)

- Loader version — both a distro loader (1.3.275) and a newer LunarG loader
  (1.4.x) fail identically without GLVND.
- Device nodes — `/dev/dri/renderD128` + `/dev/nvidia*` are injected by the
  toolkit's graphics capability and are present.
- glibc malloc-hook "undefined symbol" `LD_DEBUG` lines — they appear on a
  working host too; a red herring.
- Toolkit version — current (1.19.x) reproduces it.
- Hand bind-mounting more NVIDIA libs (`libnvidia-egl-*`, `libnvidia-api`,
  `nvvm70`) — does not help; the missing piece is the *GLVND dispatch*, not a
  vendor lib.

### How to detect in the field

`vulkaninfo --summary` reporting `ERROR_INCOMPATIBLE_DRIVER` while
`nvidia-smi` works is the signature. Confirm the dispatch libs are present
(`ldconfig -p | grep -E 'libGLdispatch|libGLX\.so|libEGL\.so'`); their absence
is the bug. Requires `--gpus all` and `NVIDIA_DRIVER_CAPABILITIES=all` (the
`graphics` + `compute` + `utility` caps) on the container.

### Reference

- Reproduced on Ubuntu 24.04, RTX 3090, driver 595.71.05 (production), NVIDIA
  Container Toolkit 1.19.x.
- `nvidia/vulkan` images are abandoned — the working recipe is a CUDA runtime
  base (`nvidia/cuda:*-runtime-ubuntu24.04`) or `ubuntu:24.04`, plus the GLVND
  set above. Sibling EGL-modifier learning:
  [`nvidia-egl-dmabuf-render-target.md`](nvidia-egl-dmabuf-render-target.md).
- Applied in [`Dockerfile`](../../Dockerfile); see [`docker/README.md`](../../docker/README.md).

## Part 2 — PipeWire in a container for an ALSA/cpal app

### Symptom

StreamLib's audio is `cpal` → **ALSA** on Linux (`packages/audio`, `cpal 0.15`).
In a container with no sound hardware, audio processors fail to open a device,
or the audio stack hangs at startup, or `pactl`/`pw-cli` time out.

### Root cause + fix

There is no `/dev/snd` and no PulseAudio. The working shape is a fully
userspace PipeWire stack with the ALSA→PipeWire bridge, so cpal's ALSA
`default` device routes into PipeWire's virtual devices:

- Install **`pipewire pipewire-bin pipewire-alsa wireplumber libspa-0.2-modules dbus`**.
  `pipewire-alsa` is the ALSA→PipeWire bridge (a packaged
  `/usr/share/alsa/alsa.conf.d/*pipewire*.conf` — do **not** hand-author it);
  `pw-cli` lives in `pipewire-bin`, not `pipewire`.
- Declare a **virtual null sink + source declaratively** in
  `/etc/pipewire/pipewire.conf.d/*.conf` (`support.null-audio-sink`) so the
  devices exist at startup with no imperative `pactl` race.
- Entrypoint order: `XDG_RUNTIME_DIR=/run/user/0` (`mkdir -m700`) → a **dbus
  session bus** (`dbus-daemon --session`, export `DBUS_SESSION_BUS_ADDRESS`) →
  `pipewire` → `wireplumber`, then **poll `pw-cli info 0`** until it answers.

### Pitfalls (each verified to bite)

- **`XDG_RUNTIME_DIR=/tmp` is rejected** — PipeWire requires a `0700` dir under
  `/run/user/<uid>`. For root-in-container that's `/run/user/0`.
- **Never run `pulseaudio` and `pipewire-pulse` together** — they fight for the
  same socket. (We don't need either; cpal uses the ALSA bridge.)
- **Use WirePlumber, not `pipewire-media-session`** — the latter is dead.
- **Poll readiness, don't `sleep`** — daemon startup time varies; a fixed
  `sleep` ladder is flaky. "Ready" means `pw-cli info 0` answers, not "the
  process exists".
- **`pipewire-alsa` is not a daemon** — it's a plugin config; install the
  package, don't try to launch a process for it.
- Skip brittle `sed` edits of packaged `.conf` files, `mkdir /dev/snd`, and
  latency tuning on a virtual sink.

### Reference

- Applied in [`docker/entrypoint.sh`](../../docker/entrypoint.sh) +
  [`docker/pipewire/10-virtual.conf`](../../docker/pipewire/10-virtual.conf).
  Genuine low-latency (drone) audio is opt-in via `--cap-add SYS_NICE
  --ulimit rtprio=95`; it degrades gracefully to non-RT without them.
