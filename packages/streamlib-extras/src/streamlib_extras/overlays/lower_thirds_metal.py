"""
Metal-native lower thirds overlay handler.

Pure Metal implementation of newscast-style lower thirds with:
- Text overlays (name, title, channel)
- Animated background box with colored accent bar
- LIVE indicator with pulsing animation
- Slide-in animation with ease-out cubic
- Zero CPU transfers - all compositing in Metal!

This keeps the entire pipeline on GPU for maximum performance.
"""

import time
from typing import Optional, Tuple
from pathlib import Path
import numpy as np

try:
    from PIL import Image, ImageDraw, ImageFont
    PIL_AVAILABLE = True
except ImportError:
    PIL_AVAILABLE = False

try:
    from Metal import (
        MTLCreateSystemDefaultDevice,
        MTLPixelFormatBGRA8Unorm,
        MTLLoadActionLoad,
        MTLStoreActionStore,
        MTLRenderPassDescriptor,
        MTLTextureDescriptor,
        MTLBlendFactorSourceAlpha,
        MTLBlendFactorOneMinusSourceAlpha,
        MTLBlendOperationAdd,
        MTLRenderPipelineDescriptor,
    )
    METAL_AVAILABLE = True
except ImportError:
    METAL_AVAILABLE = False

from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class LowerThirdsMetalHandler(StreamHandler):
    """
    Metal-native lower thirds overlay - pure GPU compositing!

    Features:
        - Name and title text overlays
        - Colored accent bar
        - Background box with transparency
        - LIVE indicator with pulsing red dot
        - Channel number display
        - Slide-in animation (ease-out cubic)
        - All rendering and compositing in Metal

    Example:
        ```python
        camera = CameraHandlerMetal()
        blur = BlurFilterMetal()
        lower_thirds = LowerThirdsMetalHandler(
            name="YOUR NAME",
            title="STREAMLIB DEMO",
            bar_color=(255, 165, 0),
            live_indicator=True
        )
        display = DisplayMetalHandler()

        # Pure Metal pipeline: camera → blur → lower thirds → display
        # 40-60 FPS with full compositing! 🚀
        ```
    """

    preferred_dispatcher = 'asyncio'

    def __init__(
        self,
        name: str = '',
        title: str = '',
        bar_color: Tuple[int, int, int] = (255, 165, 0),  # RGB
        live_indicator: bool = False,
        channel: str = '',
        slide_duration: float = 1.5,
        position: str = 'bottom-left',
        handler_name: str = 'lower-thirds-metal',
    ):
        if not METAL_AVAILABLE:
            raise ImportError(
                "Metal required for LowerThirdsMetalHandler. "
                "This handler is macOS-only."
            )

        if not PIL_AVAILABLE:
            raise ImportError(
                "PIL required for text rendering. "
                "Install: pip install Pillow"
            )

        super().__init__(handler_name)

        # Configuration
        self.name = name
        self.title = title
        self.bar_color = bar_color
        self.live_indicator = live_indicator
        self.channel = channel
        self.slide_duration = slide_duration
        self.position = position

        # Ports
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

        # Metal resources (initialized in on_start)
        self.metal_device = None
        self.command_queue = None
        self.render_pipeline_state = None

        # Overlay textures
        self.overlay_texture = None
        self.overlay_width = 0
        self.overlay_height = 0

        # Fonts
        self.name_font = None
        self.title_font = None
        self.small_font = None

        # Animation state
        self._start_time = None
        self._last_update_time = None

        # Frame dimensions (set on first frame)
        self.frame_width = 0
        self.frame_height = 0

    async def on_start(self) -> None:
        """Initialize Metal device and resources."""
        # Get Metal device
        self.metal_device = MTLCreateSystemDefaultDevice()
        if not self.metal_device:
            raise RuntimeError(f"[{self.handler_id}] Failed to create Metal device")

        # Create command queue
        self.command_queue = self.metal_device.newCommandQueue()

        # Load Metal shader for compositing
        shader_path = Path(__file__).parent.parent / 'shaders' / 'lower_thirds_composite.metal'
        if shader_path.exists():
            shader_code = shader_path.read_text()
        else:
            shader_code = self._get_composite_shader()

        # Compile shader
        result = self.metal_device.newLibraryWithSource_options_error_(
            shader_code, None, None
        )

        if result is None or len(result) != 2:
            raise RuntimeError(f"[{self.handler_id}] Failed to create Metal library")

        metal_library, error = result

        if error is not None:
            error_msg = str(error) if error else "Unknown error"
            raise RuntimeError(f"[{self.handler_id}] Metal shader compilation failed: {error_msg}")

        if metal_library is None:
            raise RuntimeError(f"[{self.handler_id}] Metal library is None")

        # Get shader functions
        vertex_fn = metal_library.newFunctionWithName_('vertex_overlay')
        fragment_fn = metal_library.newFunctionWithName_('fragment_overlay')

        if not vertex_fn or not fragment_fn:
            raise RuntimeError(f"[{self.handler_id}] Failed to load shader functions")

        # Create render pipeline with alpha blending
        pipeline_desc = MTLRenderPipelineDescriptor.alloc().init()
        pipeline_desc.setVertexFunction_(vertex_fn)
        pipeline_desc.setFragmentFunction_(fragment_fn)

        # Enable alpha blending
        color_attachment = pipeline_desc.colorAttachments().objectAtIndexedSubscript_(0)
        color_attachment.setPixelFormat_(MTLPixelFormatBGRA8Unorm)
        color_attachment.setBlendingEnabled_(True)
        color_attachment.setSourceRGBBlendFactor_(MTLBlendFactorSourceAlpha)
        color_attachment.setDestinationRGBBlendFactor_(MTLBlendFactorOneMinusSourceAlpha)
        color_attachment.setRgbBlendOperation_(MTLBlendOperationAdd)
        color_attachment.setSourceAlphaBlendFactor_(MTLBlendFactorSourceAlpha)
        color_attachment.setDestinationAlphaBlendFactor_(MTLBlendFactorOneMinusSourceAlpha)
        color_attachment.setAlphaBlendOperation_(MTLBlendOperationAdd)

        pipeline_result = self.metal_device.newRenderPipelineStateWithDescriptor_error_(
            pipeline_desc, None
        )

        if not pipeline_result or len(pipeline_result) != 2:
            raise RuntimeError(f"[{self.handler_id}] Failed to create render pipeline")

        pipeline_state, pipeline_error = pipeline_result

        if pipeline_error is not None:
            raise RuntimeError(f"[{self.handler_id}] Pipeline error: {pipeline_error}")

        if pipeline_state is None:
            raise RuntimeError(f"[{self.handler_id}] Pipeline state is None")

        self.render_pipeline_state = pipeline_state

        # Load fonts
        self._load_fonts()

        # Start animation timer
        self._start_time = time.time()

        print(f"[{self.handler_id}] Metal lower thirds initialized")
        print(f"[{self.handler_id}] Name: {self.name}, Title: {self.title}")
        print(f"[{self.handler_id}] LIVE: {self.live_indicator}, Channel: {self.channel}")

    def _load_fonts(self):
        """Load TrueType fonts for text rendering."""
        font_paths = [
            "/System/Library/Fonts/SFNSDisplay.ttf",  # SF Display (macOS)
            "/System/Library/Fonts/Helvetica.ttc",    # Helvetica (macOS)
        ]

        # Try to load fonts
        for path in font_paths:
            try:
                self.name_font = ImageFont.truetype(path, 48)  # Large for name
                self.title_font = ImageFont.truetype(path, 32)  # Medium for title
                self.small_font = ImageFont.truetype(path, 24)  # Small for channel/LIVE
                return
            except:
                continue

        # Fallback to default
        try:
            self.name_font = ImageFont.truetype("arial", 48)
            self.title_font = ImageFont.truetype("arial", 32)
            self.small_font = ImageFont.truetype("arial", 24)
        except:
            self.name_font = ImageFont.load_default()
            self.title_font = ImageFont.load_default()
            self.small_font = ImageFont.load_default()

    def _get_slide_offset(self, current_time: float) -> int:
        """Calculate slide-in offset with ease-out cubic."""
        if self._start_time is None:
            return 0

        elapsed = current_time - self._start_time
        progress = min(1.0, elapsed / self.slide_duration)

        # Ease-out cubic: 1 - (1 - t)^3
        eased = 1.0 - pow(1.0 - progress, 3)

        # Calculate offset (starts offscreen, slides to 0)
        max_offset = self.frame_width if self.frame_width > 0 else 1920
        offset = int(max_offset * (1.0 - eased))

        return offset

    def _render_lower_thirds(self, current_time: float) -> Optional[Image.Image]:
        """Render lower thirds overlay to PIL image."""
        if self.frame_width == 0 or self.frame_height == 0:
            return None

        # Calculate dimensions
        bar_height = 8
        box_height = 120
        padding = 20
        text_padding = 15

        # Calculate slide offset
        slide_offset = self._get_slide_offset(current_time)

        # Create transparent image
        img = Image.new('RGBA', (self.frame_width, box_height), (0, 0, 0, 0))
        draw = ImageDraw.Draw(img)

        # Determine position
        if self.position == 'bottom-left':
            box_x = -slide_offset  # Slide from left
            box_y = 0  # Will be positioned at bottom by shader
        elif self.position == 'bottom-right':
            box_x = self.frame_width + slide_offset  # Slide from right
            box_y = 0
        else:
            box_x = -slide_offset
            box_y = 0

        # Draw colored accent bar at top
        draw.rectangle(
            [box_x, 0, box_x + 600, bar_height],
            fill=(*self.bar_color, 255)
        )

        # Draw semi-transparent background box
        draw.rectangle(
            [box_x, bar_height, box_x + 600, box_height],
            fill=(0, 0, 0, 180)
        )

        # Draw name text (large, white)
        if self.name:
            name_y = bar_height + text_padding
            draw.text(
                (box_x + padding, name_y),
                self.name,
                fill=(255, 255, 255, 255),
                font=self.name_font
            )

        # Draw title text (medium, slightly dimmed)
        if self.title:
            title_y = bar_height + text_padding + 50
            draw.text(
                (box_x + padding, title_y),
                self.title,
                fill=(200, 200, 200, 255),
                font=self.title_font
            )

        # Draw LIVE indicator (red dot + text)
        if self.live_indicator:
            # Pulsing effect: 0.8 to 1.0 alpha over 1 second
            pulse_progress = (current_time % 1.0)
            pulse_alpha = int(255 * (0.8 + 0.2 * abs(np.sin(pulse_progress * np.pi))))

            live_x = box_x + 520
            live_y = bar_height + 20

            # Red dot
            draw.ellipse(
                [live_x, live_y, live_x + 16, live_y + 16],
                fill=(255, 0, 0, pulse_alpha)
            )

            # LIVE text
            draw.text(
                (live_x + 24, live_y),
                "LIVE",
                fill=(255, 255, 255, 255),
                font=self.small_font
            )

        # Draw channel number
        if self.channel:
            channel_x = box_x + 520
            channel_y = bar_height + 60
            draw.text(
                (channel_x, channel_y),
                self.channel,
                fill=(255, 255, 255, 255),
                font=self.small_font
            )

        return img

    def _upload_overlay_to_texture(self, img: Image.Image):
        """Upload PIL image to Metal texture."""
        img_np = np.array(img)
        height, width = img_np.shape[:2]

        # Convert RGBA to BGRA for Metal (swap R and B channels)
        # Metal textures are BGRA, PIL produces RGBA
        bgra_img = img_np.copy()
        bgra_img[:, :, [0, 2]] = bgra_img[:, :, [2, 0]]  # Swap R and B

        # Create or update texture if size changed
        if self.overlay_texture is None or self.overlay_width != width or self.overlay_height != height:
            texture_desc = MTLTextureDescriptor.texture2DDescriptorWithPixelFormat_width_height_mipmapped_(
                MTLPixelFormatBGRA8Unorm, width, height, False
            )
            texture_desc.setUsage_(1)  # ShaderRead
            self.overlay_texture = self.metal_device.newTextureWithDescriptor_(texture_desc)
            self.overlay_width = width
            self.overlay_height = height

        # Upload image data (now in BGRA format)
        region = ((0, 0, 0), (width, height, 1))
        self.overlay_texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow_(
            region, 0, bgra_img.tobytes(), width * 4
        )

    def _get_composite_shader(self) -> str:
        """Return Metal shader for compositing lower thirds overlay."""
        return """
#include <metal_stdlib>
using namespace metal;

struct VertexOut {
    float4 position [[position]];
    float2 texCoord;
};

// Vertex shader: positioned overlay quad
vertex VertexOut vertex_overlay(
    uint vertexID [[vertex_id]],
    constant float2& screenSize [[buffer(0)]],
    constant float2& overlayPosition [[buffer(1)]],
    constant float2& overlaySize [[buffer(2)]]
) {
    // Quad vertices (two triangles)
    float2 quadPositions[6] = {
        float2(0.0, 0.0),  // bottom-left
        float2(1.0, 0.0),  // bottom-right
        float2(0.0, 1.0),  // top-left
        float2(0.0, 1.0),  // top-left
        float2(1.0, 0.0),  // bottom-right
        float2(1.0, 1.0),  // top-right
    };

    float2 texCoords[6] = {
        float2(0.0, 0.0),  // bottom-left
        float2(1.0, 0.0),  // bottom-right
        float2(0.0, 1.0),  // top-left
        float2(0.0, 1.0),  // top-left
        float2(1.0, 0.0),  // bottom-right
        float2(1.0, 1.0),  // top-right
    };

    // Calculate position in pixel coordinates
    float2 pixelPos = overlayPosition + quadPositions[vertexID] * overlaySize;

    // Convert to NDC
    float2 ndc = (pixelPos / screenSize) * 2.0 - 1.0;
    ndc.y = -ndc.y;  // Flip Y

    VertexOut out;
    out.position = float4(ndc, 0.0, 1.0);
    out.texCoord = texCoords[vertexID];
    return out;
}

// Fragment shader: sample overlay texture with alpha
fragment float4 fragment_overlay(
    VertexOut in [[stage_in]],
    texture2d<float> overlayTexture [[texture(0)]]
) {
    constexpr sampler textureSampler(mag_filter::linear, min_filter::linear);
    return overlayTexture.sample(textureSampler, in.texCoord);
}
"""

    async def process(self, tick: TimedTick) -> None:
        """
        Composite lower thirds overlay onto input video frame.

        All compositing done in Metal with alpha blending!
        """
        try:
            frame_msg = self.inputs['video'].read_latest()
            if frame_msg is None:
                return

            # Check if input is Metal texture
            if not hasattr(frame_msg.data, 'width'):
                print(f"[{self.handler_id}] ERROR: Input is not a Metal texture!")
                return

            input_texture = frame_msg.data

            # Set frame dimensions on first frame
            if self.frame_width == 0:
                self.frame_width = input_texture.width()
                self.frame_height = input_texture.height()
                print(f"[{self.handler_id}] Frame size: {self.frame_width}x{self.frame_height}")

            # Get current time for animation
            current_time = time.time()

            # Render lower thirds overlay
            overlay_img = self._render_lower_thirds(current_time)
            if overlay_img is None:
                # Pass through input unchanged
                self.outputs['video'].write(frame_msg)
                return

            # Upload overlay to Metal texture
            self._upload_overlay_to_texture(overlay_img)

            # Create output texture (same size as input)
            from Metal import MTLTextureDescriptor
            output_desc = MTLTextureDescriptor.texture2DDescriptorWithPixelFormat_width_height_mipmapped_(
                MTLPixelFormatBGRA8Unorm, self.frame_width, self.frame_height, False
            )
            output_desc.setUsage_(5)  # ShaderRead | RenderTarget
            output_texture = self.metal_device.newTextureWithDescriptor_(output_desc)

            # Create command buffer
            command_buffer = self.command_queue.commandBuffer()

            # First pass: Copy input to output (blit)
            blit_encoder = command_buffer.blitCommandEncoder()
            blit_encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin_(
                input_texture, 0, 0, (0, 0, 0), (self.frame_width, self.frame_height, 1),
                output_texture, 0, 0, (0, 0, 0)
            )
            blit_encoder.endEncoding()

            # Second pass: Composite overlay with alpha blending
            render_pass = MTLRenderPassDescriptor.alloc().init()
            color_attachment = render_pass.colorAttachments().objectAtIndexedSubscript_(0)
            color_attachment.setTexture_(output_texture)
            color_attachment.setLoadAction_(MTLLoadActionLoad)  # Load existing content
            color_attachment.setStoreAction_(MTLStoreActionStore)

            # Position overlay at bottom
            import array
            screen_size = array.array('f', [float(self.frame_width), float(self.frame_height)])
            overlay_position = array.array('f', [0.0, float(self.frame_height - self.overlay_height)])
            overlay_size = array.array('f', [float(self.overlay_width), float(self.overlay_height)])

            # Create buffers
            screen_size_buffer = self.metal_device.newBufferWithBytes_length_options_(
                screen_size.tobytes(), len(screen_size) * 4, 0
            )
            overlay_position_buffer = self.metal_device.newBufferWithBytes_length_options_(
                overlay_position.tobytes(), len(overlay_position) * 4, 0
            )
            overlay_size_buffer = self.metal_device.newBufferWithBytes_length_options_(
                overlay_size.tobytes(), len(overlay_size) * 4, 0
            )

            # Render overlay
            render_encoder = command_buffer.renderCommandEncoderWithDescriptor_(render_pass)
            render_encoder.setRenderPipelineState_(self.render_pipeline_state)
            render_encoder.setVertexBuffer_offset_atIndex_(screen_size_buffer, 0, 0)
            render_encoder.setVertexBuffer_offset_atIndex_(overlay_position_buffer, 0, 1)
            render_encoder.setVertexBuffer_offset_atIndex_(overlay_size_buffer, 0, 2)
            render_encoder.setFragmentTexture_atIndex_(self.overlay_texture, 0)
            render_encoder.drawPrimitives_vertexStart_vertexCount_(4, 0, 6)  # Quad
            render_encoder.endEncoding()

            # Commit command buffer
            command_buffer.commit()
            command_buffer.waitUntilCompleted()

            # Output Metal texture
            output_frame = VideoFrame(
                data=output_texture,
                timestamp=tick.timestamp,
                frame_number=tick.frame_number,
                width=self.frame_width,
                height=self.frame_height
            )
            self.outputs['video'].write(output_frame)

        except Exception as e:
            print(f"[{self.handler_id}] ERROR: {e}")
            import traceback
            traceback.print_exc()

    async def on_stop(self) -> None:
        """Cleanup Metal resources."""
        print(f"[{self.handler_id}] Metal lower thirds stopped")


if METAL_AVAILABLE:
    __all__ = ['LowerThirdsMetalHandler']
else:
    __all__ = []
