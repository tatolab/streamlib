#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Run the full nvpro-vulkan-video test suite.
#
# Usage:
#   ./tests/run_tests.sh              # run all tests
#   ./tests/run_tests.sh --unit       # unit tests only (no GPU)
#   ./tests/run_tests.sh --gpu        # GPU tests only
#   ./tests/run_tests.sh --encode     # encode tests only
#   ./tests/run_tests.sh --decode     # decode tests only
#   ./tests/run_tests.sh --pipeline   # pipeline round-trip only

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_DIR"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0

run_test() {
    local name="$1"
    shift
    echo -e "\n${BLUE}${BOLD}--- $name ---${NC}"
    if "$@"; then
        ((PASSED++))
        echo -e "${GREEN}[PASS]${NC} $name"
    else
        local exit_code=$?
        if [[ $exit_code -eq 0 ]]; then
            ((PASSED++))
        else
            ((FAILED++))
            echo -e "${RED}[FAIL]${NC} $name (exit code $exit_code)"
        fi
    fi
}

skip_test() {
    local name="$1"
    local reason="$2"
    ((SKIPPED++))
    echo -e "\n${YELLOW}[SKIP]${NC} $name -- $reason"
}

MODE="${1:-all}"

echo -e "${BOLD}========================================${NC}"
echo -e "${BOLD}nvpro-vulkan-video Test Suite${NC}"
echo -e "${BOLD}========================================${NC}"
echo ""

# ---------------------------------------------------------------
# 1. Generate fixtures (if not present)
# ---------------------------------------------------------------
FIXTURE_DIR="$SCRIPT_DIR/fixtures"
if [[ ! -f "$FIXTURE_DIR/testsrc2_640x480_nv12.yuv" ]]; then
    echo -e "${YELLOW}Generating fixtures...${NC}"
    echo -e "${YELLOW}Requires ffmpeg with libx264/libx265. Set FFMPEG=/path/to/ffmpeg if needed.${NC}"
    "$SCRIPT_DIR/generate_fixtures.sh"
    echo ""
fi

# ---------------------------------------------------------------
# 2. Unit tests (no GPU required)
# ---------------------------------------------------------------
if [[ "$MODE" == "all" || "$MODE" == "--unit" ]]; then
    echo -e "\n${BOLD}=== Unit Tests (no GPU) ===${NC}"

    run_test "cargo test (563 unit tests)" \
        cargo test --quiet

    run_test "encode-test (115 config tests)" \
        cargo run --quiet --bin encode-test

    run_test "decode-test (parser tests)" \
        cargo run --quiet --bin decode-test
fi

# ---------------------------------------------------------------
# 3. GPU Encode tests
# ---------------------------------------------------------------
if [[ "$MODE" == "all" || "$MODE" == "--gpu" || "$MODE" == "--encode" ]]; then
    echo -e "\n${BOLD}=== GPU Encode Tests ===${NC}"

    run_test "H.264 encode (30 frames, SMPTE bars)" \
        cargo run --quiet --bin encode-test -- --gpu
fi

# ---------------------------------------------------------------
# 4. GPU Decode tests
# ---------------------------------------------------------------
if [[ "$MODE" == "all" || "$MODE" == "--gpu" || "$MODE" == "--decode" ]]; then
    echo -e "\n${BOLD}=== GPU Decode Tests ===${NC}"

    # Use known-good ffmpeg fixture for decode testing
    if [[ -f "$FIXTURE_DIR/testsrc2_640x480.h264" ]]; then
        cp "$FIXTURE_DIR/testsrc2_640x480.h264" /tmp/test_h264_stream.h264
        run_test "H.264 decode (GPU)" \
            cargo run --quiet --bin decode-test -- --gpu
    else
        skip_test "H.264 decode (GPU)" "No fixture: run ./tests/generate_fixtures.sh"
    fi
fi

# ---------------------------------------------------------------
# 5. Pipeline round-trip tests
# ---------------------------------------------------------------
if [[ "$MODE" == "all" || "$MODE" == "--gpu" || "$MODE" == "--pipeline" ]]; then
    echo -e "\n${BOLD}=== Pipeline Tests (encode -> decode) ===${NC}"

    run_test "H.264 pipeline (encode -> decode -> compare)" \
        cargo run --quiet --bin pipeline-test
fi

# ---------------------------------------------------------------
# Summary
# ---------------------------------------------------------------
TOTAL=$((PASSED + FAILED + SKIPPED))
echo -e "\n${BOLD}========================================${NC}"
echo -e "${BOLD}Test Results${NC}"
echo -e "${BOLD}========================================${NC}"
echo -e "  ${GREEN}Passed:${NC}  $PASSED"
echo -e "  ${RED}Failed:${NC}  $FAILED"
echo -e "  ${YELLOW}Skipped:${NC} $SKIPPED"
echo -e "  Total:   $TOTAL"
echo ""

if [[ $FAILED -gt 0 ]]; then
    echo -e "${RED}${BOLD}SOME TESTS FAILED${NC}"
    exit 1
else
    echo -e "${GREEN}${BOLD}ALL TESTS PASSED${NC}"
    exit 0
fi
