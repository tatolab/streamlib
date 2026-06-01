#!/usr/bin/env bash
# Final-image entrypoint for the self-contained headless StreamLib GPU service.
#
# Brings up the in-container support processes, then exec's the runtime as PID 1
# so container-stop signals reach it directly:
#   1. Gitea (as the `git` user — refuses root) serving the pre-filled package
#      registry, so runtime `add_module` of new packages resolves in-container.
#   2. The userspace audio stack (dbus session bus -> PipeWire -> WirePlumber)
#      with a declarative virtual null sink — no /dev/snd, no display server.
#   3. `exec streamlib-runtime --host 0.0.0.0` (overridable via the image CMD).
#
# Support processes are best-effort: core boot loads the pre-materialized
# api-server from cache and needs neither Gitea nor audio, so a support-process
# hiccup warns but never blocks the runtime. systemd-in-Docker is avoided by
# design; this entrypoint is the supervisor.
set -uo pipefail

GITEA_WORK_DIR="${GITEA_WORK_DIR:-/var/lib/gitea}"
GITEA_CONF="${GITEA_WORK_DIR}/conf/app.ini"
GITEA_URL="${GITEA_URL:-http://localhost:3300}"
XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/0}"
export XDG_RUNTIME_DIR

log()  { printf '[entrypoint] %s\n' "$*"; }
warn() { printf '[entrypoint] WARN: %s\n' "$*" >&2; }

# ---------------------------------------------------------------------------
# 1. In-container package registry (Gitea as the git user).
# ---------------------------------------------------------------------------
if [ -x /usr/local/bin/gitea ] && [ -f "$GITEA_CONF" ]; then
  log "starting Gitea registry ($GITEA_URL)"
  # HOME must be git-writable: Gitea's startup RewriteAllPublicKeys writes ~/.ssh,
  # and setpriv does not reset the inherited HOME=/root.
  setpriv --reuid=git --regid=git --init-groups \
    env HOME="$GITEA_WORK_DIR" GITEA_WORK_DIR="$GITEA_WORK_DIR" \
    /usr/local/bin/gitea --config "$GITEA_CONF" web >/var/log/gitea.log 2>&1 &
  for i in $(seq 1 30); do
    curl -fsS "$GITEA_URL/api/v1/version" >/dev/null 2>&1 && { log "Gitea ready"; break; }
    [ "$i" = 30 ] && warn "Gitea not ready in 30s (core boot still works; add_module of new packages may not)"
    sleep 1
  done
else
  warn "Gitea binary/config not found — in-container registry disabled"
fi

# ---------------------------------------------------------------------------
# 2. Userspace audio (dbus session bus -> PipeWire -> WirePlumber).
#    cpal -> ALSA -> PipeWire via the packaged pipewire-alsa bridge config.
# ---------------------------------------------------------------------------
if command -v pipewire >/dev/null 2>&1; then
  mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
  export DBUS_SESSION_BUS_ADDRESS="unix:path=${XDG_RUNTIME_DIR}/bus"
  dbus-daemon --session --address="$DBUS_SESSION_BUS_ADDRESS" --nofork --nopidfile >/var/log/dbus.log 2>&1 &
  sleep 0.5
  pipewire   >/var/log/pipewire.log 2>&1 &
  wireplumber >/var/log/wireplumber.log 2>&1 &
  ready=0
  for i in $(seq 1 30); do
    if pw-cli info 0 >/dev/null 2>&1; then ready=1; log "PipeWire ready"; break; fi
    sleep 0.5
  done
  [ "$ready" = 1 ] || warn "PipeWire not ready — audio processors will fail; rest of the pipeline is unaffected"
else
  warn "pipewire not installed — audio stack disabled"
fi

# ---------------------------------------------------------------------------
# 3. Optional GPU sanity line (non-fatal — useful in `docker run` logs).
# ---------------------------------------------------------------------------
if command -v vulkaninfo >/dev/null 2>&1; then
  dev="$(vulkaninfo --summary 2>/dev/null | grep -m1 deviceName || true)"
  [ -n "$dev" ] && log "GPU: ${dev#*= }" || warn "vulkaninfo found no Vulkan device (need --gpus all + NVIDIA_DRIVER_CAPABILITIES=all)"
fi

log "starting: $*"
exec "$@"
