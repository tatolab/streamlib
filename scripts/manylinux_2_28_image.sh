# shellcheck shell=bash disable=SC2034
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Sourced by every script that builds inside manylinux_2_28. A `docker run` of
# this image sets STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE to it, which is how a
# script inside the container knows it already runs on the pinned image.

# manylinux_2_28 as of 2026-08-04, pinned so a release builds against the image
# its gate last passed on.
MANYLINUX_2_28_IMAGE="quay.io/pypa/manylinux_2_28_x86_64@sha256:012f4a50472412f18bb2b450c1cce7158434cfae4ae878591c2748a13a30c2be"
