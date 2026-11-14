# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Video compositing nodes for Tatolab streams, built on the public compute-kernel surface."""

from .alpha_over_compositor import ALPHA_OVER_LAYER_COUNT as ALPHA_OVER_LAYER_COUNT
from .alpha_over_compositor import AlphaOverCompositor as AlphaOverCompositor
from .alpha_over_compositor import AlphaOverCompositorConfig as AlphaOverCompositorConfig
from .alpha_over_compositor import AlphaOverLayerPlacement as AlphaOverLayerPlacement

__all__ = [
    "ALPHA_OVER_LAYER_COUNT",
    "AlphaOverCompositor",
    "AlphaOverCompositorConfig",
    "AlphaOverLayerPlacement",
]
