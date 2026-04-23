#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# StreamLib Installer
# Builds streamlib + streamlib-runtime from source and installs them into
# ~/.streamlib/bin.
#
# Usage:
#   curl -sSf https://streamlib.dev/install.sh | sh
#   curl -sSf https://streamlib.dev/install.sh | sh -s -- --version 0.4.21

set -euo pipefail

STREAMLIB_HOME="${HOME}/.streamlib"
GITHUB_REPO="tatolab/streamlib"
VERSION=""

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
    --version VERSION    Build a specific tag/branch (default: main)
    --help               Show this help
EOF
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --version)
                VERSION="$2"
                shift 2
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

check_platform() {
    local os
    os="$(uname -s)"
    case "$os" in
        Darwin|Linux) ;;
        *) error "Unsupported platform: $os"; exit 1 ;;
    esac
}

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
        git fetch --depth 1 origin "v${VERSION}" || git fetch --depth 1 origin "${VERSION}"
        git checkout "v${VERSION}" 2>/dev/null || git checkout "${VERSION}"
    fi

    info "Building streamlib + streamlib-runtime (this may take a few minutes)..."
    cd "$tmp_dir/streamlib"
    cargo build --release -p streamlib-cli -p streamlib-runtime

    if [[ -z "$VERSION" ]]; then
        VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    fi

    local version_dir="${STREAMLIB_HOME}/versions/${VERSION}"
    mkdir -p "$version_dir"
    cp "target/release/streamlib" "$version_dir/"
    cp "target/release/streamlib-runtime" "$version_dir/"
    chmod 755 "$version_dir/streamlib" "$version_dir/streamlib-runtime"

    success "Installed to ${version_dir}"
}

create_symlinks() {
    local bin_dir="${STREAMLIB_HOME}/bin"
    local version_dir="${STREAMLIB_HOME}/versions/${VERSION}"

    mkdir -p "$bin_dir"
    ln -sf "$version_dir/streamlib" "$bin_dir/streamlib"
    ln -sf "$version_dir/streamlib-runtime" "$bin_dir/streamlib-runtime"

    success "Created symlinks in ${bin_dir}"
}

setup_shell() {
    local env_file="${STREAMLIB_HOME}/env"

    cat > "$env_file" << 'EOF'
# StreamLib environment
# Add this to your shell rc file:
#   . "$HOME/.streamlib/env"

export PATH="$HOME/.streamlib/bin:$PATH"
EOF

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

main() {
    parse_args "$@"

    echo ""
    echo "StreamLib Installer"
    echo "==================="
    echo ""

    check_platform
    build_from_source
    create_symlinks
    setup_shell
}

main "$@"
