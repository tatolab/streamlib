# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The skia overlay, drawn into a raster surface instead of an engine texture.

The drawing code is deliberately free of streamlib, so it can be checked here
for the things that make it composite correctly downstream: a transparent
background, premultiplied edges, and an animation that actually moves.
"""

from __future__ import annotations

import numpy
import pytest
import skia

from camera_python_effects.neon_overlay_canvas import (
    OVERLAY_ALPHA_TYPE,
    OVERLAY_COLOR_TYPE,
    SLIDE_IN_SECONDS,
    draw_neon_overlay,
    ease_out_back,
)

OVERLAY_WIDTH = 640
OVERLAY_HEIGHT = 360


def overlay_at(elapsed_seconds: float) -> numpy.ndarray:
    surface = skia.Surface.MakeRaster(
        skia.ImageInfo.Make(
            OVERLAY_WIDTH, OVERLAY_HEIGHT, OVERLAY_COLOR_TYPE, OVERLAY_ALPHA_TYPE
        )
    )
    with surface as canvas:
        draw_neon_overlay(canvas, OVERLAY_WIDTH, OVERLAY_HEIGHT, elapsed_seconds)
    return numpy.array(surface.makeImageSnapshot())


@pytest.fixture(scope="module")
def settled_overlay() -> numpy.ndarray:
    return overlay_at(SLIDE_IN_SECONDS * 2)


def test_the_overlay_has_its_own_shape(settled_overlay: numpy.ndarray) -> None:
    assert settled_overlay.shape == (OVERLAY_HEIGHT, OVERLAY_WIDTH, 4)
    assert settled_overlay.dtype == numpy.uint8


def test_the_overlay_actually_draws_something(settled_overlay: numpy.ndarray) -> None:
    """A missing font or a path off-canvas would leave this fully transparent."""
    assert numpy.count_nonzero(settled_overlay[:, :, 3]) > 0


def test_most_of_the_frame_stays_transparent(settled_overlay: numpy.ndarray) -> None:
    """It is a layer, not a picture — an opaque background would hide the video."""
    covered_fraction = numpy.count_nonzero(settled_overlay[:, :, 3]) / (
        OVERLAY_WIDTH * OVERLAY_HEIGHT
    )
    assert covered_fraction < 0.5


def test_the_pixels_are_premultiplied(settled_overlay: numpy.ndarray) -> None:
    """The compositor's blend is `src.rgb + dst.rgb * (1 - src.a)`, so a colour
    channel above its own alpha would brighten wherever the layer is soft."""
    colour = settled_overlay[:, :, :3].astype(numpy.int16)
    alpha = settled_overlay[:, :, 3].astype(numpy.int16)[:, :, None]
    assert numpy.all(colour <= alpha)


def test_the_lower_third_slides_in() -> None:
    """It has to be somewhere different at the start than at the end."""
    at_the_start = overlay_at(0.0)
    at_the_end = overlay_at(SLIDE_IN_SECONDS * 2)
    assert not numpy.array_equal(at_the_start, at_the_end)
    assert numpy.count_nonzero(at_the_start[:, :, 3]) < numpy.count_nonzero(
        at_the_end[:, :, 3]
    )


def test_the_watermark_drips_move() -> None:
    assert not numpy.array_equal(overlay_at(10.0), overlay_at(11.4))


def test_the_slide_easing_overshoots_and_settles() -> None:
    assert ease_out_back(0.0) == pytest.approx(0.0)
    assert ease_out_back(1.0) == pytest.approx(1.0)
    assert max(ease_out_back(step / 100.0) for step in range(101)) > 1.0
