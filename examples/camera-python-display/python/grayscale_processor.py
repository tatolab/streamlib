# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Passthrough video processor with IOSurface access.

This processor demonstrates video frame passthrough with access to IOSurface
IDs for cross-framework GPU sharing (e.g., SceneKit, Core Image, Metal).

Also demonstrates custom schema definition that gets registered in
the Rust SCHEMA_REGISTRY.
"""

import logging

from streamlib import processor, input, output, schema, f32, i64, bool_field

# Get a logger for this module - logs will be forwarded to Rust tracing
logger = logging.getLogger(__name__)


# =============================================================================
# Example: Custom schema definition (registered in Rust SCHEMA_REGISTRY)
# =============================================================================

@schema(name="TestEmbedding")
class TestEmbeddingSchema:
    """Example custom schema for ML embedding output.

    This schema is registered in Rust's SCHEMA_REGISTRY when this module
    is imported, making it available for inspection via the API server.
    """
    embedding = f32(shape=[512], description="Feature embedding vector")
    timestamp = i64(description="Timestamp in nanoseconds")
    confidence = f32(description="Confidence score 0.0-1.0")
    active = bool_field(description="Whether detection is active")


# =============================================================================
# Passthrough Processor (IOSurface demo)
# =============================================================================

@processor(name="GrayscaleProcessor", description="Passthrough processor with IOSurface access")
class GrayscaleProcessor:
    """Passes video frames through with IOSurface ID logging.

    This demonstrates the IOSurface sharing pattern for cross-framework
    GPU access. The IOSurface ID can be used with:

    - SceneKit: Create Metal texture from IOSurface for 3D scene rendering
    - Core Image: Apply CIFilter effects via GPU
    - Metal: Direct Metal compute/render operations
    - AVFoundation: Video composition and effects

    Example SceneKit usage (requires pyobjc-framework-SceneKit):

        from IOSurface import IOSurfaceLookup
        from Metal import MTLCreateSystemDefaultDevice

        # Get shared IOSurface from frame
        iosurface_id = frame.iosurface_id  # Available on pooled textures
        surface = IOSurfaceLookup(iosurface_id)

        # Create Metal texture from IOSurface
        device = MTLCreateSystemDefaultDevice()
        descriptor = metal.MTLTextureDescriptor.texture2DDescriptor(...)
        texture = device.newTextureWithDescriptor_iosurface_plane_(
            descriptor, surface, 0
        )

        # Use texture in SceneKit scene
        material = SceneKit.SCNMaterial.alloc().init()
        material.diffuse().setContents_(texture)
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize processor."""
        self.frame_count = 0
        logger.info("Setup complete (passthrough mode)")
        logger.debug("GPU shader processing removed - use native Rust processors or SceneKit via PyObjC")

    def process(self, ctx):
        """Pass each frame through, logging IOSurface ID."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        # Log frame info periodically
        self.frame_count += 1
        if self.frame_count % 60 == 0:  # Log every 60 frames
            width = frame["width"]
            height = frame["height"]
            logger.debug(f"Frame {self.frame_count} - {width}x{height}")

        # Pass frame through unmodified
        # To modify frames, use:
        # 1. Native Rust processors (recommended for performance)
        # 2. SceneKit/Core Image via PyObjC (for Python-native effects)
        # 3. acquire_surface() to get IOSurface-backed output textures
        ctx.output("video_out").set(frame)

    def teardown(self, ctx):
        """Cleanup on shutdown."""
        logger.info(f"Processed {self.frame_count} frames")
        logger.info("Teardown complete")
