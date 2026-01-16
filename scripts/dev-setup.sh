#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# StreamLib Developer Setup
# Creates a local dev environment with proxy scripts that use cargo run.
#
# Usage:
#   ./scripts/dev-setup.sh              # Setup local dev environment
#   ./scripts/dev-setup.sh uninstall    # Uninstall dev broker and clean up
#   ./scripts/dev-setup.sh reinstall    # Uninstall then reinstall (rebuilds broker)
#   ./scripts/dev-setup.sh --clean      # Alias for reinstall

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STREAMLIB_HOME="${REPO_ROOT}/.streamlib"
BROKER_PORT=50052

# Generate short hash from full path (supports multiple worktrees)
PATH_HASH="$(echo -n "$REPO_ROOT" | shasum | cut -c1-6)"
SERVICE_LABEL="Streamlib-dev-${PATH_HASH}"
SERVICE_NAME="com.tatolab.streamlib.broker.dev-${PATH_HASH}"
LOG_FILE="/tmp/streamlib-broker-dev-${PATH_HASH}.log"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { echo -e "${BLUE}==>${NC} $1"; }
success() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}==>${NC} $1"; }
error() { echo -e "${RED}==>${NC} $1" >&2; }

# Check platform
check_platform() {
    if [[ "$(uname)" != "Darwin" ]]; then
        error "StreamLib broker is currently macOS-only"
        exit 1
    fi
}

# Check dependencies
check_dependencies() {
    if ! command -v cargo &> /dev/null; then
        error "Rust/Cargo not found. Install from https://rustup.rs"
        exit 1
    fi
}

# Uninstall existing installation
uninstall() {
    info "Uninstalling existing dev installation..."

    info "Stopping broker service..."
    local domain="gui/$(id -u)"
    launchctl bootout "$domain/${SERVICE_LABEL}" 2>/dev/null || true
    info "Removing launchd plist..."
    rm -f "${HOME}/Library/LaunchAgents/${SERVICE_NAME}.plist"
    info "Removing .streamlib directory..."
    rm -rf "${STREAMLIB_HOME}"
    info "Removing cargo config env vars..."
    remove_cargo_config_env

    success "Uninstalled"
}

# Update .cargo/config.toml with dev environment variables
update_cargo_config() {
    local config_file="${REPO_ROOT}/.cargo/config.toml"

    info "Updating .cargo/config.toml with dev environment..."

    # First remove any existing StreamLib env vars
    remove_cargo_config_env

    # Append StreamLib env vars to the [env] section
    # We add them at the end of the file since [env] section exists
    cat >> "$config_file" << EOF

# StreamLib dev environment (managed by dev-setup.sh - DO NOT EDIT)
STREAMLIB_HOME = "${STREAMLIB_HOME}"
STREAMLIB_BROKER_PORT = "${BROKER_PORT}"
STREAMLIB_DEV_MODE = "1"
STREAMLIB_BROKER_XPC_SERVICE = "${SERVICE_NAME}"
# End StreamLib dev environment
EOF

    success "Updated .cargo/config.toml"
}

# Remove StreamLib env vars from .cargo/config.toml
remove_cargo_config_env() {
    local config_file="${REPO_ROOT}/.cargo/config.toml"

    if [[ ! -f "$config_file" ]]; then
        return 0
    fi

    # Remove lines between markers (inclusive)
    # Using sed to delete from marker start to marker end
    sed -i '' '/^# StreamLib dev environment/,/^# End StreamLib dev environment/d' "$config_file" 2>/dev/null || true

    # Also remove any trailing empty lines that might be left
    # This is a bit tricky with sed, so we'll just leave them
}

# Create proxy scripts
create_proxy_scripts() {
    local bin_dir="${STREAMLIB_HOME}/bin"

    info "Creating proxy scripts..."

    mkdir -p "$bin_dir"

    # Get cargo path for launchd environment
    local cargo_bin
    cargo_bin="$(dirname "$(which cargo)")"

    # CLI proxy script
    cat > "$bin_dir/streamlib" << EOF
#!/usr/bin/env bash
# StreamLib CLI proxy - calls cargo run for dev mode
set -euo pipefail

export PATH="${cargo_bin}:\$PATH"
SOURCE_ROOT="${REPO_ROOT}"
BROKER_PORT=${BROKER_PORT}

export STREAMLIB_HOME="${STREAMLIB_HOME}"
export STREAMLIB_BROKER_PORT="\$BROKER_PORT"
export STREAMLIB_DEV_MODE=1
export STREAMLIB_BROKER_XPC_SERVICE="${SERVICE_NAME}"

exec cargo run --manifest-path "\$SOURCE_ROOT/Cargo.toml" -p streamlib-cli --quiet -- "\$@"
EOF
    chmod 755 "$bin_dir/streamlib"
    success "Created CLI proxy: $bin_dir/streamlib"

    # Broker proxy script
    cat > "$bin_dir/streamlib-broker" << EOF
#!/usr/bin/env bash
# StreamLib Broker proxy - calls cargo run for dev mode
set -euo pipefail

export PATH="/opt/homebrew/bin:${cargo_bin}:\$PATH"
SOURCE_ROOT="${REPO_ROOT}"
BROKER_PORT=${BROKER_PORT}

export STREAMLIB_HOME="${STREAMLIB_HOME}"
export STREAMLIB_DEV_MODE=1
export STREAMLIB_BROKER_XPC_SERVICE="${SERVICE_NAME}"

exec cargo run --manifest-path "\$SOURCE_ROOT/Cargo.toml" -p streamlib-broker --quiet -- --port "\$BROKER_PORT" "\$@"
EOF
    chmod 755 "$bin_dir/streamlib-broker"
    success "Created broker proxy: $bin_dir/streamlib-broker"
}

# Generate and install launchd plist
install_plist() {
    local plist_path="${HOME}/Library/LaunchAgents/${SERVICE_NAME}.plist"
    local broker_path="${STREAMLIB_HOME}/bin/streamlib-broker"

    info "Creating launchd plist..."

    mkdir -p "${HOME}/Library/LaunchAgents"

    cat > "$plist_path" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${SERVICE_LABEL}</string>
    <key>MachServices</key>
    <dict>
        <key>${SERVICE_NAME}</key>
        <true/>
    </dict>
    <key>ProgramArguments</key>
    <array>
        <string>${broker_path}</string>
    </array>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${LOG_FILE}</string>
    <key>StandardErrorPath</key>
    <string>${LOG_FILE}</string>
    <key>WorkingDirectory</key>
    <string>${REPO_ROOT}</string>
</dict>
</plist>
EOF

    success "Created ${plist_path}"
}

# Start broker service
start_broker() {
    local plist_path="${HOME}/Library/LaunchAgents/${SERVICE_NAME}.plist"
    local domain="gui/$(id -u)"

    info "Starting broker service..."

    # Bootstrap the service
    launchctl bootstrap "$domain" "$plist_path" 2>/dev/null || true

    # Wait for it to start (cargo run takes longer)
    info "Waiting for broker to compile and start..."
    sleep 5

    # Verify
    if launchctl list "$SERVICE_LABEL" &>/dev/null; then
        success "Broker service started"
    else
        warn "Broker may not have started. Check: $LOG_FILE"
    fi
}

# Verify installation
verify() {
    info "Verifying installation..."

    local cli="${STREAMLIB_HOME}/bin/streamlib"

    # Give broker more time to start on first run
    local max_attempts=10
    local attempt=1

    while [[ $attempt -le $max_attempts ]]; do
        if "$cli" broker status &>/dev/null; then
            success "Broker is healthy!"
            echo ""
            "$cli" broker status
            break
        fi
        info "Waiting for broker... (attempt $attempt/$max_attempts)"
        sleep 2
        ((attempt++))
    done

    if [[ $attempt -gt $max_attempts ]]; then
        warn "Broker status check failed. Check: $LOG_FILE"
        echo ""
        echo "Last 20 lines of log:"
        tail -20 "$LOG_FILE" 2>/dev/null || echo "(no log file yet)"
    fi

    echo ""
    info "Dev environment:"
    echo "  Location:     ${STREAMLIB_HOME}"
    echo "  Broker port:  ${BROKER_PORT}"
    echo "  Service:      ${SERVICE_LABEL}"
    echo "  Path hash:    ${PATH_HASH}"
    echo "  Log file:     ${LOG_FILE}"
    echo ""
    info "Proxy scripts:"
    ls -la "${STREAMLIB_HOME}/bin/"
}

# Install (setup) the dev environment
install() {
    create_proxy_scripts
    update_cargo_config
    install_plist
    start_broker
    verify

    echo ""
    success "Dev setup complete!"
    echo ""
    echo "Use the plugin commands or run directly:"
    echo "  ./.streamlib/bin/streamlib broker status"
    echo ""
}

# Main
main() {
    echo ""
    echo "StreamLib Developer Setup (Local Dev Mode)"
    echo "==========================================="
    echo ""

    check_platform
    check_dependencies

    local cmd="${1:-install}"

    case "$cmd" in
        uninstall)
            uninstall
            ;;
        reinstall|--clean)
            uninstall
            install
            ;;
        install|"")
            install
            ;;
        *)
            error "Unknown command: $cmd"
            echo ""
            echo "Usage:"
            echo "  ./scripts/dev-setup.sh              # Setup local dev environment"
            echo "  ./scripts/dev-setup.sh uninstall    # Uninstall dev broker and clean up"
            echo "  ./scripts/dev-setup.sh reinstall    # Uninstall then reinstall (rebuilds broker)"
            exit 1
            ;;
    esac
}

main "$@"
