#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

# Build script for streamlib-python development.
# Creates venv if needed and builds the package with maturin.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== StreamLib Python Development Build ==="
echo ""

# Check for uv
if ! command -v uv &> /dev/null; then
    echo "Error: uv is not installed. Install with: curl -LsSf https://astral.sh/uv/install.sh | sh"
    exit 1
fi

# Check for maturin
if ! command -v maturin &> /dev/null; then
    echo "Error: maturin is not installed. Install with: uv tool install maturin"
    exit 1
fi

# Create venv if it doesn't exist
if [ ! -d ".venv" ]; then
    echo "Creating virtual environment..."
    uv venv
    echo ""
fi

# Activate venv
echo "Activating virtual environment..."
source .venv/bin/activate

# Build and install with maturin
echo "Building with maturin..."
echo ""
maturin develop

echo ""
echo "=== Build Complete ==="
echo ""
echo "The streamlib package is now installed in .venv"
echo "To use it interactively: source .venv/bin/activate"
echo ""
echo "To run examples with this venv, ensure venv_path is set in the config."
