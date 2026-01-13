#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# StreamLib Developer Setup
# Builds from source and installs the broker for local development.
#
# Usage:
#   ./scripts/dev-setup.sh          # Build and install
#   ./scripts/dev-setup.sh --clean  # Uninstall first, then reinstall

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
STREAMLIB_HOME="${HOME}/.streamlib"

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

# Build broker and CLI
build() {
    info "Building streamlib-broker and streamlib-cli (release)..."
    cd "$REPO_ROOT"
    cargo build --release -p streamlib-broker -p streamlib-cli
    success "Build complete"
}

# Uninstall existing broker
uninstall() {
    info "Uninstalling existing broker..."

    # Stop service if running
    local domain="gui/$(id -u)"
    launchctl bootout "$domain/com.tatolab.streamlib.broker" 2>/dev/null || true

    # Remove files
    rm -f "${HOME}/Library/LaunchAgents/com.tatolab.streamlib.broker.plist"
    rm -rf "${STREAMLIB_HOME}/bin"
    rm -rf "${STREAMLIB_HOME}/versions"

    success "Uninstalled"
}

# Install broker
install_broker() {
    local broker_binary="${REPO_ROOT}/target/release/streamlib-broker"

    # Get version from workspace Cargo.toml
    local version
    version=$(grep '^version' "${REPO_ROOT}/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')

    local version_dir="${STREAMLIB_HOME}/versions/${version}"
    local bin_dir="${STREAMLIB_HOME}/bin"

    info "Installing broker v${version}..."

    # Create directories
    mkdir -p "$version_dir"
    mkdir -p "$bin_dir"

    # Copy binary
    cp "$broker_binary" "$version_dir/streamlib-broker"
    chmod 755 "$version_dir/streamlib-broker"

    # Create symlink
    ln -sf "$version_dir/streamlib-broker" "$bin_dir/streamlib-broker"

    success "Installed to ${version_dir}"
}

# Generate and install launchd plist
install_plist() {
    local plist_path="${HOME}/Library/LaunchAgents/com.tatolab.streamlib.broker.plist"
    local broker_path="${STREAMLIB_HOME}/bin/streamlib-broker"

    info "Creating launchd plist..."

    mkdir -p "${HOME}/Library/LaunchAgents"

    cat > "$plist_path" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.tatolab.streamlib.broker</string>
    <key>ProgramArguments</key>
    <array>
        <string>${broker_path}</string>
    </array>
    <key>MachServices</key>
    <dict>
        <key>com.tatolab.streamlib.runtime</key>
        <true/>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/streamlib-broker.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/streamlib-broker.log</string>
</dict>
</plist>
EOF

    success "Created ${plist_path}"
}

# Start broker service
start_broker() {
    local plist_path="${HOME}/Library/LaunchAgents/com.tatolab.streamlib.broker.plist"
    local domain="gui/$(id -u)"

    info "Starting broker service..."

    # Bootstrap the service
    launchctl bootstrap "$domain" "$plist_path" 2>/dev/null || true

    # Wait for it to start
    sleep 1

    # Verify
    if launchctl list com.tatolab.streamlib.broker &>/dev/null; then
        success "Broker service started"
    else
        warn "Broker may not have started. Check: /tmp/streamlib-broker.log"
    fi
}

# Setup shell environment
setup_shell() {
    local env_file="${STREAMLIB_HOME}/env"

    info "Creating shell environment file..."

    cat > "$env_file" << 'EOF'
# StreamLib environment
# Add this to your shell rc file:
#   . "$HOME/.streamlib/env"

export PATH="$HOME/.streamlib/bin:$PATH"
EOF

    success "Created ${env_file}"

    # Detect shell and give instructions
    local shell_name
    shell_name=$(basename "$SHELL")
    local rc_file

    case "$shell_name" in
        zsh)  rc_file="~/.zshrc" ;;
        bash) rc_file="~/.bashrc" ;;
        fish) rc_file="~/.config/fish/config.fish" ;;
        *)    rc_file="your shell rc file" ;;
    esac

    echo ""
    echo "Add this line to ${rc_file}:"
    echo ""
    echo "  . \"\$HOME/.streamlib/env\""
    echo ""
    echo "Then reload your shell or run:"
    echo ""
    echo "  source ${rc_file}"
    echo ""
}

# Verify installation
verify() {
    info "Verifying installation..."

    local cli="${REPO_ROOT}/target/release/streamlib"

    if "$cli" broker status &>/dev/null; then
        success "Broker is healthy!"
        echo ""
        "$cli" broker status
    else
        warn "Broker status check failed. Check /tmp/streamlib-broker.log"
    fi
}

# Main
main() {
    echo ""
    echo "StreamLib Developer Setup"
    echo "========================="
    echo ""

    check_platform
    check_dependencies

    # Handle --clean flag
    if [[ "${1:-}" == "--clean" ]]; then
        uninstall
    fi

    build
    install_broker
    install_plist
    start_broker
    setup_shell
    verify

    echo ""
    success "Setup complete!"
    echo ""
}

main "$@"
