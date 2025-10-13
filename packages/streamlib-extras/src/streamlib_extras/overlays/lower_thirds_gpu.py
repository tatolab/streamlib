"""
GPU-accelerated lower thirds graphics overlay handler.

Draws newscast-style lower thirds entirely on GPU with slide-in animation.
"""

import numpy as np
from typing import Tuple, Optional

try:
    import torch
    TORCH_AVAILABLE = True
except ImportError:
    TORCH_AVAILABLE = False

try:
    from PIL import Image, ImageDraw, ImageFont
    PIL_AVAILABLE = True
except ImportError:
    PIL_AVAILABLE = False

from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class LowerThirdsGPUHandler(StreamHandler):
    """
    GPU-accelerated newscast-style lower thirds overlay with slide-in animation.

    Draws a professional lower thirds graphic that slides in from the right,
    then stays visible. All rendering and compositing done on GPU.

    Features:
    - Slide-in animation (configurable duration, ease-out cubic)
    - Two-line text (name + title/subtitle)
    - Colored accent bar
    - Optional "LIVE" indicator
    - Optional channel/number display
    - GPU-accelerated rendering

    Example:
        ```python
        lower_thirds = LowerThirdsGPUHandler(
            name="SARAH MCFINN",
            title="ULTIMATE FIGHTER",
            bar_color=(255, 165, 0),  # Orange in RGB
            live_indicator=True,
            channel="45"
        )
        runtime.connect(source.outputs['video'], lower_thirds.inputs['video'])
        runtime.connect(lower_thirds.outputs['video'], display.inputs['video'])
        ```
    """

    preferred_dispatcher = 'asyncio'  # GPU operations are non-blocking

    def __init__(
        self,
        name: str = "JOHN DOE",
        title: str = "TITLE HERE",
        bar_color: Tuple[int, int, int] = (255, 165, 0),  # Orange RGB
        text_color: Tuple[int, int, int] = (255, 255, 255),  # White
        bg_color: Tuple[int, int, int] = (40, 40, 80),  # Dark blue-gray
        live_indicator: bool = True,
        live_color: Tuple[int, int, int] = (255, 0, 0),  # Red RGB
        channel: Optional[str] = None,
        slide_duration: float = 1.0,  # Seconds to slide in
        position: str = "bottom-left",  # or "bottom-right", "top-left", "top-right"
        handler_id: str = None
    ):
        """
        Initialize GPU-accelerated lower thirds overlay handler.

        Args:
            name: Primary text (top line, larger)
            title: Secondary text (bottom line, smaller)
            bar_color: Color for accent bar (RGB)
            text_color: Color for text (RGB)
            bg_color: Color for background box (RGB)
            live_indicator: Show "LIVE" indicator
            live_color: Color for LIVE indicator (RGB)
            channel: Optional channel number/text
            slide_duration: Duration of slide-in animation (seconds)
            position: Position on screen (bottom-left, bottom-right, top-left, top-right)
            handler_id: Optional custom handler ID
        """
        if not TORCH_AVAILABLE:
            raise ImportError("PyTorch required for GPU lower thirds")
        if not PIL_AVAILABLE:
            raise ImportError("PIL required for text rendering")

        super().__init__(handler_id or 'lower-thirds-gpu')

        self.name = name.upper()
        self.title = title.upper()
        self.bar_color = bar_color
        self.text_color = text_color
        self.bg_color = bg_color
        self.live_indicator = live_indicator
        self.live_color = live_color
        self.channel = channel
        self.slide_duration = slide_duration
        self.position = position

        # GPU-capable ports
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

        # Animation state
        self._start_time: Optional[float] = None
        self._animation_complete = False

        # Cached GPU overlay texture (created on first frame)
        self._overlay_texture: Optional[torch.Tensor] = None
        self._overlay_mask: Optional[torch.Tensor] = None
        self._box_width = 0
        self._box_height = 0

        # Device
        self.device = None

    def _calculate_dimensions(self, frame_width: int, frame_height: int):
        """Calculate lower thirds dimensions based on frame size."""
        self._box_width = int(frame_width * 0.35)  # 35% of frame width
        self._box_height = int(frame_height * 0.15)  # 15% of frame height

    def _create_overlay_texture(self, frame_width: int, frame_height: int):
        """
        Pre-render lower thirds overlay to GPU texture.

        Creates an RGBA texture with the lower thirds graphics that can be
        composited onto video frames.
        """
        if self._box_width == 0:
            self._calculate_dimensions(frame_width, frame_height)

        # Create PIL image for text rendering
        overlay_img = Image.new('RGBA', (self._box_width, self._box_height), (0, 0, 0, 0))
        draw = ImageDraw.Draw(overlay_img)

        # Draw background box
        draw.rectangle(
            [(0, 0), (self._box_width, self._box_height)],
            fill=(*self.bg_color, 255)
        )

        # Draw accent bar (left side)
        bar_width = int(self._box_width * 0.02)
        draw.rectangle(
            [(0, 0), (bar_width, self._box_height)],
            fill=(*self.bar_color, 255)
        )

        # Load fonts
        try:
            name_font = ImageFont.truetype("/System/Library/Fonts/SFCompact.ttf", 28)
            title_font = ImageFont.truetype("/System/Library/Fonts/SFCompact.ttf", 18)
            live_font = ImageFont.truetype("/System/Library/Fonts/SFCompactRounded.ttf", 16)
            channel_font = ImageFont.truetype("/System/Library/Fonts/SFCompact.ttf", 40)
        except:
            # Fallback
            name_font = ImageFont.load_default()
            title_font = ImageFont.load_default()
            live_font = ImageFont.load_default()
            channel_font = ImageFont.load_default()

        # Draw name (larger text)
        text_x = bar_width + 20
        name_y = int(self._box_height * 0.25)
        draw.text((text_x, name_y), self.name, fill=(*self.text_color, 255), font=name_font)

        # Draw title (smaller text, colored)
        title_y = int(self._box_height * 0.60)
        draw.text((text_x, title_y), self.title, fill=(*self.bar_color, 255), font=title_font)

        # Draw LIVE indicator
        if self.live_indicator:
            live_x = self._box_width - 80
            live_y = 15

            # Red circle
            circle_radius = 6
            draw.ellipse(
                [(live_x - circle_radius, live_y - circle_radius),
                 (live_x + circle_radius, live_y + circle_radius)],
                fill=(*self.live_color, 255)
            )

            # "LIVE" text
            draw.text((live_x + 10, live_y - 8), "LIVE", fill=(*self.text_color, 255), font=live_font)

        # Draw channel number
        if self.channel:
            channel_x = self._box_width - 60
            channel_y = self._box_height - 50
            draw.text((channel_x, channel_y), self.channel, fill=(*self.text_color, 255), font=channel_font)

        # Convert to numpy then torch
        overlay_np = np.array(overlay_img)  # RGBA

        # Split into RGB and alpha
        rgb = overlay_np[:, :, :3]  # [H, W, 3]
        alpha = overlay_np[:, :, 3:4]  # [H, W, 1]

        # Convert to torch tensors and move to device
        rgb_tensor = torch.from_numpy(rgb).to(self.device)
        alpha_tensor = torch.from_numpy(alpha).to(self.device).float() / 255.0

        self._overlay_texture = rgb_tensor
        self._overlay_mask = alpha_tensor

    def _get_slide_offset(self, current_time: float, frame_width: int) -> int:
        """
        Calculate horizontal offset for slide-in animation.

        Returns:
            Pixel offset from target position (0 = fully visible)
        """
        if self._animation_complete:
            return 0

        if self._start_time is None:
            self._start_time = current_time

        # Calculate animation progress (0.0 to 1.0)
        elapsed = current_time - self._start_time
        progress = min(1.0, elapsed / self.slide_duration)

        if progress >= 1.0:
            self._animation_complete = True
            return 0

        # Ease-out cubic
        eased = 1.0 - pow(1.0 - progress, 3)

        # Start from right edge of screen
        max_offset = frame_width
        offset = int(max_offset * (1.0 - eased))

        return offset

    async def on_start(self):
        """Initialize GPU device."""
        if self._runtime and self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            print(f"[{self.handler_id}] GPU lower thirds initialized: '{self.name}' / '{self.title}'")
        else:
            self.device = torch.device('cpu')
            print(f"[{self.handler_id}] Lower thirds using CPU (no GPU context)")

    async def process(self, tick: TimedTick):
        """Composite lower thirds overlay on video frame."""
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data

        # Convert to torch tensor if needed
        if not isinstance(frame, torch.Tensor):
            frame = torch.from_numpy(frame).to(self.device)

        # Ensure frame is on correct device
        if frame.device != self.device:
            frame = frame.to(self.device)

        h, w = frame.shape[:2]

        # Create overlay texture on first frame
        if self._overlay_texture is None:
            self._create_overlay_texture(w, h)

        # Get animation offset
        slide_offset = self._get_slide_offset(tick.timestamp, w)

        # Calculate box position based on position setting
        margin = 40
        if self.position.startswith("bottom"):
            box_y = h - self._box_height - margin
        else:  # top
            box_y = margin

        if self.position.endswith("left"):
            box_x = margin + slide_offset
        else:  # right
            box_x = w - self._box_width - margin + slide_offset

        # Only composite if box is visible
        if box_x < w:
            # Calculate visible region
            visible_width = min(self._box_width, w - box_x)
            y_end = min(box_y + self._box_height, h)
            x_end = box_x + visible_width

            # Get frame region
            frame_region = frame[box_y:y_end, box_x:x_end]

            # Get overlay region (may be clipped)
            overlay_h = y_end - box_y
            overlay_w = x_end - box_x
            overlay_rgb = self._overlay_texture[:overlay_h, :overlay_w]
            overlay_alpha = self._overlay_mask[:overlay_h, :overlay_w]

            # Alpha blend: result = overlay * alpha + frame * (1 - alpha)
            blended = (overlay_rgb * overlay_alpha + frame_region * (1 - overlay_alpha)).byte()

            # Write back to frame
            frame[box_y:y_end, box_x:x_end] = blended

        # Create output message
        video_frame = VideoFrame(
            data=frame,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=w,
            height=h,
            metadata=frame_msg.metadata
        )

        self.outputs['video'].write(video_frame)

    async def on_stop(self):
        """Cleanup GPU resources."""
        self._overlay_texture = None
        self._overlay_mask = None
        print(f"[{self.handler_id}] GPU lower thirds stopped")
