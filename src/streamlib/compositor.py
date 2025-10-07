"""
Video compositor for combining multiple layers.

This module provides the DefaultCompositor which combines layers using
zero-copy numpy operations and alpha blending.
"""

import numpy as np
from numpy.typing import NDArray
from typing import Optional
import skia
from .base import Compositor, Layer, TimestampedFrame
from .plugins import register_compositor


@register_compositor('default')
class DefaultCompositor(Compositor):
    """
    Default compositor that combines layers using alpha blending.

    This compositor:
    - Sorts layers by z_index (lowest to highest)
    - Generates background if no base layer
    - Alpha blends each layer on top
    - Uses zero-copy numpy operations where possible
    """

    def __init__(
        self,
        width: int = 1920,
        height: int = 1080,
        background_color: tuple[int, int, int, int] = (20, 20, 30, 255)
    ):
        """
        Initialize compositor.

        Args:
            width: Output width
            height: Output height
            background_color: RGBA background color tuple
        """
        super().__init__(width, height)
        self.background_color = background_color
        self.frame_number = 0

    async def composite(
        self,
        input_frame: Optional[TimestampedFrame] = None
    ) -> TimestampedFrame:
        """
        Composite all layers into a single output frame.

        Args:
            input_frame: Optional input frame to pass to layers

        Returns:
            Composited TimestampedFrame with all layers blended
        """
        # Start with background
        background = self._generate_background()

        # Get sorted layers
        sorted_layers = self._sort_layers()

        # If no layers, show placeholder
        if not sorted_layers:
            background = self._add_placeholder(background)
            return TimestampedFrame(
                frame=background,
                timestamp=input_frame.timestamp if input_frame else 0.0,
                frame_number=self.frame_number,
                ptp_time=input_frame.ptp_time if input_frame else None,
                source_id=input_frame.source_id if input_frame else None
            )

        # Composite each layer
        result = background
        for layer in sorted_layers:
            # Process layer to get RGBA overlay
            overlay = await layer.process_frame(input_frame, self.width, self.height)

            # Apply layer opacity
            if layer.opacity < 1.0:
                overlay = self._apply_opacity(overlay, layer.opacity)

            # Alpha blend onto result
            result = self._alpha_blend(result, overlay)

        self.frame_number += 1

        return TimestampedFrame(
            frame=result,
            timestamp=input_frame.timestamp if input_frame else 0.0,
            frame_number=self.frame_number,
            ptp_time=input_frame.ptp_time if input_frame else None,
            source_id=input_frame.source_id if input_frame else None
        )

    def _generate_background(self) -> NDArray[np.uint8]:
        """
        Generate background pattern.

        Returns:
            RGBA background frame
        """
        frame = np.zeros((self.height, self.width, 4), dtype=np.uint8)

        # Dark gradient background
        for y in range(self.height):
            intensity = int(self.background_color[0] + (y / self.height) * 30)
            frame[y, :] = [
                intensity,
                intensity,
                self.background_color[2] + int(y / self.height * 10),
                self.background_color[3]
            ]

        return frame

    def _add_placeholder(self, frame: NDArray[np.uint8]) -> NDArray[np.uint8]:
        """
        Add placeholder graphic when no layers are present.

        Args:
            frame: Background frame

        Returns:
            Frame with placeholder graphic
        """
        # Create Skia surface from numpy array
        surface = skia.Surface(self.width, self.height)
        canvas = surface.getCanvas()

        # Draw frame as background - Skia expects RGBA
        rgba_frame = np.ascontiguousarray(frame)
        image = skia.Image.fromarray(rgba_frame)
        canvas.drawImage(image, 0, 0)

        paint = skia.Paint()
        paint.setAntiAlias(True)

        # Centered box
        box_width = min(800, self.width - 100)
        box_height = min(400, self.height - 100)
        box_x = (self.width - box_width) / 2
        box_y = (self.height - box_height) / 2

        # Semi-transparent dark panel
        paint.setColor(skia.Color(30, 30, 40, 220))
        canvas.drawRoundRect(
            skia.Rect(box_x, box_y, box_x + box_width, box_y + box_height),
            20, 20, paint
        )

        # Border
        paint.setStyle(skia.Paint.kStroke_Style)
        paint.setStrokeWidth(3)
        paint.setColor(skia.Color(100, 100, 120, 180))
        canvas.drawRoundRect(
            skia.Rect(box_x, box_y, box_x + box_width, box_y + box_height),
            20, 20, paint
        )

        # Reset to fill style
        paint.setStyle(skia.Paint.kFill_Style)

        # Animated pulsing circle
        pulse = 0.8 + 0.2 * np.sin(self.frame_number / 15.0 * np.pi)
        circle_radius = 40 * pulse
        circle_x = self.width / 2
        circle_y = box_y + 100

        paint.setColor(skia.Color(76, 175, 80, int(200 * pulse)))
        canvas.drawCircle(circle_x, circle_y, circle_radius, paint)

        # Title
        paint.setColor(skia.Color(200, 200, 210))
        font_title = skia.Font(None, 48)
        text = "Waiting for Content..."
        text_width = font_title.measureText(text)
        canvas.drawString(
            text,
            (self.width - text_width) / 2,
            box_y + 200,
            font_title,
            paint
        )

        # Subtitle
        paint.setColor(skia.Color(150, 150, 160))
        font_subtitle = skia.Font(None, 24)
        subtext = "Add a layer to begin streaming"
        subtext_width = font_subtitle.measureText(subtext)
        canvas.drawString(
            subtext,
            (self.width - subtext_width) / 2,
            box_y + 240,
            font_subtitle,
            paint
        )

        # Frame counter (subtle)
        paint.setColor(skia.Color(100, 100, 110, 150))
        font_counter = skia.Font(None, 18)
        counter_text = f"Frame {self.frame_number}"
        canvas.drawString(
            counter_text,
            box_x + 20,
            box_y + box_height - 30,
            font_counter,
            paint
        )

        # Convert back to numpy
        result = surface.makeImageSnapshot().toarray()
        return result

    def _apply_opacity(
        self,
        frame: NDArray[np.uint8],
        opacity: float
    ) -> NDArray[np.uint8]:
        """
        Apply opacity to a frame.

        Args:
            frame: RGBA frame
            opacity: Opacity value (0.0 to 1.0)

        Returns:
            Frame with opacity applied
        """
        result = frame.copy()
        result[:, :, 3] = (result[:, :, 3] * opacity).astype(np.uint8)
        return result

    def _alpha_blend(
        self,
        background: NDArray[np.uint8],
        overlay: NDArray[np.uint8]
    ) -> NDArray[np.uint8]:
        """
        Alpha blend overlay onto background using zero-copy numpy operations.

        Args:
            background: Background RGBA frame
            overlay: Overlay RGBA frame

        Returns:
            Blended RGBA frame
        """
        if overlay.shape != background.shape:
            # Resize overlay to match background if needed
            import cv2
            overlay = cv2.resize(overlay, (self.width, self.height))

        # Extract alpha channel and normalize to 0.0-1.0
        # Using in-place operations and views to minimize copies
        alpha = overlay[:, :, 3:4].astype(np.float32) / 255.0

        # Blend RGB channels
        # Formula: result = overlay_rgb * alpha + background_rgb * (1 - alpha)
        overlay_rgb = overlay[:, :, :3].astype(np.float32)
        background_rgb = background[:, :, :3].astype(np.float32)

        blended_rgb = (
            overlay_rgb * alpha + background_rgb * (1.0 - alpha)
        ).astype(np.uint8)

        # Composite alpha channels
        # Formula: result_alpha = overlay_alpha + background_alpha * (1 - overlay_alpha)
        bg_alpha = background[:, :, 3:4].astype(np.float32) / 255.0
        result_alpha = (alpha + bg_alpha * (1.0 - alpha)) * 255.0
        result_alpha = result_alpha.astype(np.uint8)

        # Combine RGB and alpha
        result = np.dstack([blended_rgb, result_alpha])

        return result
