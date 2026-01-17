#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# StreamLib Installer
# Downloads and installs the StreamLib broker service.
#
# Usage:
#   curl -sSf https://streamlib.dev/install.sh | sh
#   curl -sSf https://streamlib.dev/install.sh | sh -s -- --version 0.2.4
#
# Options:
#   --version VERSION    Install specific version (default: latest)
#   --from-source        Build from source instead of downloading
#   --help               Show this help

set -euo pipefail

STREAMLIB_HOME="${HOME}/.streamlib"
GITHUB_REPO="tatolab/streamlib"
VERSION=""
FROM_SOURCE=false

# Colors (only if terminal supports it)
if [[ -t 1 ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

info() { echo -e "${BLUE}==>${NC} $1"; }
success() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}==>${NC} $1"; }
error() { echo -e "${RED}error:${NC} $1" >&2; }

usage() {
    cat << EOF
StreamLib Installer

Usage:
    curl -sSf https://streamlib.dev/install.sh | sh
    curl -sSf https://streamlib.dev/install.sh | sh -s -- [OPTIONS]

Options:
    --version VERSION    Install specific version (default: latest)
    --from-source        Build from source (requires Rust)
    --help               Show this help

Examples:
    # Install latest version
    curl -sSf https://streamlib.dev/install.sh | sh

    # Install specific version
    curl -sSf https://streamlib.dev/install.sh | sh -s -- --version 0.2.4

    # Build from source
    curl -sSf https://streamlib.dev/install.sh | sh -s -- --from-source
EOF
}

# Parse arguments
parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                VERSION="$2"
                shift 2
                ;;
            --from-source)
                FROM_SOURCE=true
                shift
                ;;
            --help|-h)
                usage
                exit 0
                ;;
            *)
                error "Unknown option: $1"
                usage
                exit 1
                ;;
        esac
    done
}

# Check platform
check_platform() {
    local os
    os="$(uname -s)"

    case "$os" in
        Darwin)
            PLATFORM="macos"
            ;;
        Linux)
            error "Linux support coming soon"
            exit 1
            ;;
        *)
            error "Unsupported platform: $os"
            exit 1
            ;;
    esac

    # Check architecture
    local arch
    arch="$(uname -m)"

    case "$arch" in
        x86_64)
            ARCH="x86_64"
            ;;
        arm64|aarch64)
            ARCH="aarch64"
            ;;
        *)
            error "Unsupported architecture: $arch"
            exit 1
            ;;
    esac
}

# Get latest version from GitHub
get_latest_version() {
    if [[ -n "$VERSION" ]]; then
        return
    fi

    info "Fetching latest version..."

    if command -v curl &>/dev/null; then
        VERSION=$(curl -sSf "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep '"tag_name"' \
            | sed 's/.*"v\([^"]*\)".*/\1/' || echo "")
    elif command -v wget &>/dev/null; then
        VERSION=$(wget -qO- "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" \
            | grep '"tag_name"' \
            | sed 's/.*"v\([^"]*\)".*/\1/' || echo "")
    fi

    if [[ -z "$VERSION" ]]; then
        error "Could not determine latest version. Use --version to specify."
        exit 1
    fi

    info "Latest version: ${VERSION}"
}

# Download pre-built binary
download_binary() {
    local version_dir="${STREAMLIB_HOME}/versions/${VERSION}"
    local binary_path="${version_dir}/streamlib-broker"
    local download_url="https://github.com/${GITHUB_REPO}/releases/download/v${VERSION}/streamlib-broker-${PLATFORM}-${ARCH}"

    info "Downloading streamlib-broker v${VERSION}..."

    mkdir -p "$version_dir"

    if command -v curl &>/dev/null; then
        if ! curl -sSfL -o "$binary_path" "$download_url"; then
            error "Download failed. Binary may not be available for this platform/version."
            error "Try --from-source to build from source."
            exit 1
        fi
    elif command -v wget &>/dev/null; then
        if ! wget -q -O "$binary_path" "$download_url"; then
            error "Download failed."
            exit 1
        fi
    else
        error "curl or wget required"
        exit 1
    fi

    chmod 755 "$binary_path"
    success "Downloaded to ${binary_path}"
}

# Build from source
build_from_source() {
    if ! command -v cargo &>/dev/null; then
        error "Rust/Cargo not found. Install from https://rustup.rs"
        exit 1
    fi

    if ! command -v git &>/dev/null; then
        error "Git not found"
        exit 1
    fi

    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf '$tmp_dir'" EXIT

    info "Cloning streamlib..."
    git clone --depth 1 "https://github.com/${GITHUB_REPO}.git" "$tmp_dir/streamlib"

    if [[ -n "$VERSION" ]]; then
        cd "$tmp_dir/streamlib"
        git fetch --depth 1 origin "v${VERSION}"
        git checkout "v${VERSION}"
    fi

    info "Building streamlib-broker (this may take a few minutes)..."
    cd "$tmp_dir/streamlib"
    cargo build --release -p streamlib-broker

    # Get version from workspace Cargo.toml if not specified
    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi

    local version_dir="${STREAMLIB_HOME}/versions/${VERSION}"
    mkdir -p "$version_dir"
    cp "target/release/streamlib-broker" "$version_dir/"
    chmod 755 "$version_dir/streamlib-broker"

    success "Built and installed to ${version_dir}"
}

# Create symlink
create_symlink() {
    local bin_dir="${STREAMLIB_HOME}/bin"
    local version_dir="${STREAMLIB_HOME}/versions/${VERSION}"

    mkdir -p "$bin_dir"
    ln -sf "$version_dir/streamlib-broker" "$bin_dir/streamlib-broker"

    success "Created symlink"
}

# Install launchd plist (macOS)
install_plist() {
    if [[ "$PLATFORM" != "macos" ]]; then
        return
    fi

    local plist_path="${HOME}/Library/LaunchAgents/com.tatolab.streamlib.broker.plist"
    local broker_path="${STREAMLIB_HOME}/bin/streamlib-broker"

    info "Installing launchd service..."

    # Stop existing service
    local domain="gui/$(id -u)"
    launchctl bootout "$domain/com.tatolab.streamlib.broker" 2>/dev/null || true

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

    # Start service
    launchctl bootstrap "$domain" "$plist_path" 2>/dev/null || true
    sleep 1

    if launchctl list com.tatolab.streamlib.broker &>/dev/null; then
        success "Broker service started"
    else
        warn "Broker may not have started. Check: /tmp/streamlib-broker.log"
    fi
}

# Setup shell
setup_shell() {
    local env_file="${STREAMLIB_HOME}/env"

    cat > "$env_file" << 'EOF'
# StreamLib environment
# Add this to your shell rc file:
#   . "$HOME/.streamlib/env"

export PATH="$HOME/.streamlib/bin:$PATH"
EOF

    # Detect shell
    local shell_name
    shell_name=$(basename "${SHELL:-/bin/bash}")
    local rc_file

    case "$shell_name" in
        zsh)  rc_file="~/.zshrc" ;;
        bash) rc_file="~/.bashrc" ;;
        fish) rc_file="~/.config/fish/config.fish" ;;
        *)    rc_file="your shell rc file" ;;
    esac

    echo ""
    success "StreamLib installed successfully!"
    echo ""
    echo "Add this line to ${rc_file}:"
    echo ""
    echo "  . \"\$HOME/.streamlib/env\""
    echo ""
    echo "Then start a new shell or run:"
    echo ""
    echo "  source ${rc_file}"
    echo ""
}

# Main
main() {
    parse_args "$@"

    echo ""
    echo "StreamLib Installer"
    echo "==================="
    echo ""

    check_platform

    if [[ "$FROM_SOURCE" == true ]]; then
        build_from_source
    else
        get_latest_version
        download_binary
    fi

    create_symlink
    install_plist
    setup_shell
}

main "$@"
