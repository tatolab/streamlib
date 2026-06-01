#!/usr/bin/env bash
# Host prerequisites for running the StreamLib GPU container.
#
# A Dockerfile cannot bake these in — they touch the kernel and the Docker
# daemon, which are shared with the host:
#   1. an NVIDIA driver (the container reuses the host's; never install one
#      inside the image),
#   2. the NVIDIA Container Toolkit wired into Docker (so `--gpus all` injects
#      the driver libs + /dev/dri + /dev/nvidia*),
#   3. optional v4l2loopback virtual camera node(s) for hardware-free camera
#      input (one module, many nodes — one per container).
#
# Idempotent and configure-by-env. Re-run freely.
#
# Env:
#   INSTALL_TOOLKIT   if "1", apt-install nvidia-container-toolkit (needs sudo + apt)
#   CONFIGURE_DOCKER  if "1" (default), run `nvidia-ctk runtime configure` + restart docker
#   V4L2_NODES        space-separated video_nr list to create via v4l2loopback (e.g. "10 11")
#   V4L2_LABEL        card label prefix for the virtual cameras (default streamlib-cam)
set -euo pipefail

INSTALL_TOOLKIT="${INSTALL_TOOLKIT:-0}"
CONFIGURE_DOCKER="${CONFIGURE_DOCKER:-1}"
V4L2_NODES="${V4L2_NODES:-}"
V4L2_LABEL="${V4L2_LABEL:-streamlib-cam}"

log()  { printf '[host-prereqs] %s\n' "$*"; }
warn() { printf '[host-prereqs] WARN: %s\n' "$*" >&2; }
fail() { printf '[host-prereqs] ERROR: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"

# --- 1. NVIDIA driver -------------------------------------------------------
have docker || fail "docker not installed — install Docker Engine first"
if have nvidia-smi; then
  log "NVIDIA driver: $(nvidia-smi --query-gpu=driver_version,name --format=csv,noheader | head -1)"
else
  fail "nvidia-smi not found — install the NVIDIA driver on the host (the image must NOT ship one)"
fi

# --- 2. NVIDIA Container Toolkit -------------------------------------------
if have nvidia-ctk; then
  log "nvidia-container-toolkit present: $(nvidia-ctk --version 2>/dev/null | head -1)"
elif [ "$INSTALL_TOOLKIT" = 1 ]; then
  log "installing nvidia-container-toolkit"
  curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
    | $SUDO gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
  curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
    | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
    | $SUDO tee /etc/apt/sources.list.d/nvidia-container-toolkit.list >/dev/null
  $SUDO apt-get update && $SUDO apt-get install -y nvidia-container-toolkit
else
  warn "nvidia-container-toolkit not found — re-run with INSTALL_TOOLKIT=1 (needs sudo + apt) or install it manually"
fi

if [ "$CONFIGURE_DOCKER" = 1 ] && have nvidia-ctk; then
  log "wiring the NVIDIA runtime into Docker"
  $SUDO nvidia-ctk runtime configure --runtime=docker
  $SUDO systemctl restart docker 2>/dev/null || $SUDO service docker restart 2>/dev/null || \
    warn "could not restart docker automatically — restart it yourself for the runtime change to take effect"
fi

# --- 3. Virtual camera nodes (optional) ------------------------------------
if [ -n "$V4L2_NODES" ]; then
  have modprobe || fail "modprobe not available"
  if ! modinfo v4l2loopback >/dev/null 2>&1; then
    warn "v4l2loopback module not installed — apt-get install v4l2loopback-dkms (then re-run)"
  else
    # exclusive_caps=0 (NOT 1) — caps=1 breaks ffmpeg/streamlib writes into the node.
    log "loading v4l2loopback nodes: $V4L2_NODES"
    $SUDO modprobe -r v4l2loopback 2>/dev/null || true
    # shellcheck disable=SC2086
    $SUDO modprobe v4l2loopback video_nr=$(echo "$V4L2_NODES" | tr ' ' ',') \
      card_label="$V4L2_LABEL" exclusive_caps=0
    for n in $V4L2_NODES; do
      [ -e "/dev/video$n" ] && log "  /dev/video$n ready" || warn "  /dev/video$n not created"
    done
  fi
fi

# --- 4. Verify GPU passthrough ---------------------------------------------
log "verifying GPU passthrough (nvidia-smi inside a container)"
if docker run --rm --gpus all nvidia/cuda:13.2.1-base-ubuntu24.04 nvidia-smi -L 2>/dev/null; then
  log "GPU passthrough OK"
else
  warn "GPU passthrough check failed — confirm the toolkit is wired and docker was restarted"
fi

log "done. Build + run:  docker compose up --build   (or see docker/README.md)"
