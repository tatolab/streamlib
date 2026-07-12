#!/usr/bin/env bash
# Final-image entrypoint for the self-contained headless StreamLib GPU service.
#
# Brings up the in-container support processes, then exec's the runtime as PID 1
# so container-stop signals reach it directly:
#   1. A dumb static HTTP mount (`python3 -m http.server`) over the image-local
#      registry tree, so runtime `add_module` module builds resolve cargo
#      (sparse is HTTP-only by spec) and deno npm deps offline. pypi + `.slpkg`
#      read the same tree over `file://` and need no server. No registry daemon.
#   2. The userspace audio stack (dbus session bus -> PipeWire -> WirePlumber)
#      with a declarative virtual null sink — no /dev/snd, no display server.
#   3. `exec streamlib-runtime --host 0.0.0.0` (overridable via the image CMD).
#
# Support processes are best-effort: core boot loads the pre-materialized
# api-server from cache and needs neither the registry mount nor audio, so a
# support-process hiccup warns but never blocks the runtime. systemd-in-Docker
# is avoided by design; this entrypoint is the supervisor.
set -uo pipefail

REGISTRY_DIR="${STREAMLIB_REGISTRY_DIR:-/opt/streamlib/registry}"
REGISTRY_PORT="${STREAMLIB_REGISTRY_HTTP_PORT:-8799}"
XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/0}"
export XDG_RUNTIME_DIR

log()  { printf '[entrypoint] %s\n' "$*"; }
warn() { printf '[entrypoint] WARN: %s\n' "$*" >&2; }

# The runtime needs a writable XDG_RUNTIME_DIR for its surface-share socket,
# independent of whether the audio stack comes up below.
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"

# ---------------------------------------------------------------------------
# 1. Static registry HTTP mount (cargo + npm — sparse + npm are HTTP-only). The
#    cargo [source] replacement in $CARGO_HOME/config.toml and /root/.npmrc both
#    point at http://127.0.0.1:$REGISTRY_PORT; pypi + `.slpkg` resolve the same
#    tree over `file://` with no server. Dumb static file server, no daemon.
# ---------------------------------------------------------------------------
if [ -d "$REGISTRY_DIR" ] && command -v python3 >/dev/null 2>&1; then
  log "serving image-local static registry ($REGISTRY_DIR) on 127.0.0.1:$REGISTRY_PORT"
  python3 -m http.server "$REGISTRY_PORT" --bind 127.0.0.1 --directory "$REGISTRY_DIR" \
    >/var/log/streamlib-registry-httpd.log 2>&1 &
  for i in $(seq 1 30); do
    curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/cargo/config.json" >/dev/null 2>&1 && { log "registry mount ready"; break; }
    [ "$i" = 30 ] && warn "registry mount not ready in 30s (core boot still works; add_module of new packages may not)"
    sleep 1
  done
else
  warn "registry tree/python3 not found — in-container package resolution disabled"
fi

# ---------------------------------------------------------------------------
# 2. Userspace audio (dbus session bus -> PipeWire -> WirePlumber).
#    cpal -> ALSA -> PipeWire via the packaged pipewire-alsa bridge config.
# ---------------------------------------------------------------------------
if command -v pipewire >/dev/null 2>&1; then
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
