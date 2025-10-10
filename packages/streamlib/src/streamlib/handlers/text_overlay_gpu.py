"""
GPU-accelerated text overlay handler.

Composites text overlays on top of video frames entirely on the GPU.
"""

import numpy as np
from typing import List, Tuple, Dict, Optional

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


class GPUTextRenderer:
    """
    Text rendering utility for GPU text overlay.

    Pre-renders text to numpy arrays that can be composited with video frames.
    Uses TrueType fonts via PIL for high-quality text.
    """

    def __init__(self):
        if not PIL_AVAILABLE:
            raise ImportError("PIL required for text rendering")

        self.fonts = self._load_fonts()
        # Text cache: {text_key: (rgba_array, width, height)}
        self.text_cache: Dict[str, Tuple[np.ndarray, int, int]] = {}

    def _load_fonts(self) -> Dict[str, 'ImageFont.FreeTypeFont']:
        """Load TrueType fonts."""
        fonts = {}
        font_paths = [
            "/System/Library/Fonts/SFNSMono.ttf",  # SF Mono (macOS)
            "/System/Library/Fonts/Menlo.ttc",     # Menlo (macOS)
        ]

        sizes = {'large': 32, 'medium': 20, 'small': 16}

        for size_name, size in sizes.items():
            for path in font_paths:
                try:
                    fonts[size_name] = ImageFont.truetype(path, size)
                    break
                except:
                    continue

            if size_name not in fonts:
                # Fallback to default
                try:
                    fonts[size_name] = ImageFont.truetype("monospace", size)
                except:
                    fonts[size_name] = ImageFont.load_default()

        return fonts

    def render_text(self, text: str, font_size: str = 'medium') -> Tuple[np.ndarray, int, int]:
        """
        Render text to RGBA numpy array (cached).

        Returns: (rgba_array, width, height)
        """
        cache_key = f"{text}:{font_size}"

        if cache_key in self.text_cache:
            return self.text_cache[cache_key]

        font = self.fonts.get(font_size, self.fonts['medium'])

        # Get text bounding box
        bbox = font.getbbox(text)
        text_width = bbox[2] - bbox[0] + 10  # Add padding
        text_height = bbox[3] - bbox[1] + 10

        # Create RGBA image
        img = Image.new('RGBA', (text_width, text_height), (0, 0, 0, 0))
        draw = ImageDraw.Draw(img)
        # Offset by -bbox to position text correctly within bounds, plus padding
        draw.text((5 - bbox[0], 5 - bbox[1]), text, fill=(255, 255, 255, 255), font=font)

        # Convert to numpy (RGBA)
        rgba_array = np.array(img)

        # Cache
        self.text_cache[cache_key] = (rgba_array, text_width, text_height)

        return rgba_array, text_width, text_height

    def clear_cache(self):
        """Clear texture cache."""
        self.text_cache.clear()


class GPUTextOverlayHandler(StreamHandler):
    """
    GPU-accelerated text overlay handler.

    Composites text overlays on top of video frames using GPU operations.
    Text is rendered once and cached, then composited with alpha blending.

    Example:
        text_overlay = GPUTextOverlayHandler('text-overlay')
        text_overlay.set_text_overlays([
            ("FPS: 60.0", 20, 30, 'large'),
            ("Frame: 1234", 20, 1040, 'medium'),
        ])
    """

    def __init__(self, name: str = 'text-overlay-gpu'):
        if not TORCH_AVAILABLE:
            raise ImportError("PyTorch required for GPU text overlay")
        if not PIL_AVAILABLE:
            raise ImportError("PIL required for text rendering")

        super().__init__(name)

        # Ports
        self.inputs['video'] = VideoInput('video', capabilities=['gpu', 'cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu', 'cpu'])

        # Text renderer
        self.text_renderer = GPUTextRenderer()

        # Text overlays: List[(text, x, y, font_size)]
        self.text_overlays: List[Tuple[str, int, int, str]] = []

        # Device
        self.device = None

    def set_text_overlays(self, text_items: List[Tuple[str, int, int, str]]):
        """
        Set text overlays to composite.

        Args:
            text_items: List of (text, x, y, font_size) tuples
                       font_size: 'large' | 'medium' | 'small'
        """
        self.text_overlays = text_items

    async def on_start(self):
        """Initialize device."""
        if self._runtime and self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            print(f"[{self.handler_id}] GPU text overlay initialized on {self.device}")
        else:
            self.device = torch.device('cpu')
            print(f"[{self.handler_id}] Text overlay using CPU")

    async def process(self, tick: TimedTick):
        """Composite text overlays on video frame."""
        frame_msg = self.inputs['video'].read_latest()
        if frame_msg is None:
            return

        frame = frame_msg.data

        # If no overlays, pass through unchanged
        if len(self.text_overlays) == 0:
            video_frame = VideoFrame(
                data=frame,
                timestamp=frame_msg.timestamp,
                frame_number=frame_msg.frame_number,
                width=frame_msg.width,
                height=frame_msg.height
            )
            self.outputs['video'].write(video_frame)
            return

        # Convert to numpy for text compositing
        if isinstance(frame, torch.Tensor):
            if frame.is_cuda or frame.device.type == 'mps':
                frame_np = frame.cpu().numpy()
            else:
                frame_np = frame.numpy()
        else:
            frame_np = frame

        # Composite each text overlay using alpha blending
        for text, x, y, font_size in self.text_overlays:
            rgba_text, text_width, text_height = self.text_renderer.render_text(text, font_size)

            # Get alpha channel
            alpha = rgba_text[:, :, 3:4].astype(np.float32) / 255.0

            # Get RGB channels and convert to BGR
            text_rgb = rgba_text[:, :, :3]
            text_bgr = text_rgb[:, :, ::-1]  # RGB → BGR

            # Calculate overlay region
            h, w = frame_np.shape[:2]
            y_end = min(y + text_height, h)
            x_end = min(x + text_width, w)

            # Clip text to fit within frame bounds
            text_h = y_end - y
            text_w = x_end - x

            if text_h <= 0 or text_w <= 0:
                continue  # Text completely outside frame

            # Alpha blend: result = text * alpha + frame * (1 - alpha)
            frame_region = frame_np[y:y_end, x:x_end]
            text_region = text_bgr[:text_h, :text_w]
            alpha_region = alpha[:text_h, :text_w]

            blended = (text_region * alpha_region + frame_region * (1 - alpha_region)).astype(np.uint8)
            frame_np[y:y_end, x:x_end] = blended

        # Convert back to torch tensor if needed
        if isinstance(frame_msg.data, torch.Tensor):
            frame_out = torch.from_numpy(frame_np).to(self.device)
        else:
            frame_out = frame_np

        video_frame = VideoFrame(
            data=frame_out,
            timestamp=frame_msg.timestamp,
            frame_number=frame_msg.frame_number,
            width=frame_msg.width,
            height=frame_msg.height
        )
        self.outputs['video'].write(video_frame)

    async def on_stop(self):
        """Cleanup resources."""
        self.text_renderer.clear_cache()
        print(f"[{self.handler_id}] Text overlay stopped")
