# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

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
