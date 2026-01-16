# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Avatar Character Processor - 3D cyberpunk character for PiP overlay.

Uses MediaPipe Tasks API (GPU-accelerated) for pose detection,
then renders a 3D rigged character driven by pose landmarks.
The character is displayed in a picture-in-picture overlay with
a "Breaking News" style.

Features:
- MediaPipe PoseLandmarker with GPU/Metal delegate (33 3D landmarks)
- 3D rigged character from GLB file (Mixamo skeleton)
- GPU-accelerated skinned mesh rendering via ModernGL
- Real-time pose-to-bone rotation conversion
- Optional 3D scene background
- "Ready" state signaling for slide-in animation timing
"""

import logging
import os
import sys
import time
from pathlib import Path

# Delay heavy imports to avoid GIL deadlock during parallel module loading
moderngl = None
np = None
CharacterRenderer3D = None

def _lazy_import():
    """Import heavy dependencies lazily to avoid import-time GIL issues."""
    global moderngl, np, CharacterRenderer3D
    if moderngl is None:
        import moderngl as _moderngl
        import numpy as _np
        moderngl = _moderngl
        np = _np

        # Ensure local modules can be imported
        _module_dir = Path(__file__).parent
        if str(_module_dir) not in sys.path:
            sys.path.insert(0, str(_module_dir))

        from character_renderer_3d import CharacterRenderer3D as _CR3D
        CharacterRenderer3D = _CR3D

from streamlib import processor, input, output, PixelFormat

logger = logging.getLogger(__name__)

# Model configuration
MODEL_DIR = Path(__file__).parent / "models"
POSE_MODEL_PATH = MODEL_DIR / "pose_landmarker_lite.task"
POSE_MODEL_URL = "https://storage.googleapis.com/mediapipe-models/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task"

# Assets configuration
ASSETS_DIR = Path(__file__).parent.parent / "assets"
BACKGROUND_PATH = ASSETS_DIR / "alley.jpg"
CHARACTER_PATH = ASSETS_DIR / "character" / "character.glb"


def ensure_model_downloaded():
    """Download pose landmarker model if not present."""
    MODEL_DIR.mkdir(parents=True, exist_ok=True)

    if not POSE_MODEL_PATH.exists():
        logger.info(f"Downloading pose model to {POSE_MODEL_PATH}...")
        try:
            import urllib.request
            urllib.request.urlretrieve(POSE_MODEL_URL, POSE_MODEL_PATH)
            logger.info(f"Downloaded pose model ({POSE_MODEL_PATH.stat().st_size / 1024 / 1024:.1f} MB)")
        except Exception as e:
            logger.error(f"Failed to download pose model: {e}")
            return False
    return POSE_MODEL_PATH.exists()


# Try to import and initialize MediaPipe Tasks API
MEDIAPIPE_AVAILABLE = False
mp = None
PoseLandmarker = None
PoseLandmarkerOptions = None
BaseOptions = None
VisionRunningMode = None

try:
    import mediapipe as mp
    from mediapipe.tasks import python as mp_tasks
    from mediapipe.tasks.python import vision
    from mediapipe.tasks.python.vision import PoseLandmarker, PoseLandmarkerOptions
    from mediapipe.tasks.python.components.containers import NormalizedLandmark

    BaseOptions = mp_tasks.BaseOptions
    VisionRunningMode = vision.RunningMode

    MEDIAPIPE_AVAILABLE = True
    logger.info(f"MediaPipe Tasks API loaded (version: {mp.__version__})")
except ImportError as e:
    logger.warning(f"MediaPipe Tasks API import failed: {e}")
except Exception as e:
    logger.warning(f"MediaPipe initialization error: {e}")


# =============================================================================
# Avatar Character Processor (3D Rendering Mode)
# =============================================================================

@processor(
    name="AvatarCharacter",
    description="3D pose-tracking cyberpunk character for PiP overlay",
)
class AvatarCharacter:
    """Renders a 3D rigged character driven by MediaPipe pose landmarks.

    Uses MediaPipe Tasks API with GPU delegate for pose detection.
    Renders a 3D character model with GPU skinning via ModernGL.
    Signals "ready" state when first pose is detected (for slide-in animation).
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize MediaPipe Tasks API and 3D rendering resources."""
        # Lazy import heavy dependencies
        _lazy_import()

        self.frame_count = 0
        self.pose_landmarker = None
        self._timestamp_ms = 0
        self._mediapipe_available = MEDIAPIPE_AVAILABLE

        # Ready state - becomes True 1.5 seconds after resources are loaded
        self._is_ready = False
        self._setup_complete_time = None  # Set when 3D renderer is initialized
        self._ready_delay_seconds = 1.5  # Wait 1.5 seconds after load before sliding in

        # Ensure model is downloaded
        if self._mediapipe_available:
            if not ensure_model_downloaded():
                logger.error("AvatarCharacter: Failed to download pose model")
                self._mediapipe_available = False

        # Initialize MediaPipe Tasks API with GPU delegate
        if self._mediapipe_available:
            try:
                options = PoseLandmarkerOptions(
                    base_options=BaseOptions(
                        model_asset_path=str(POSE_MODEL_PATH),
                        delegate=BaseOptions.Delegate.GPU,
                    ),
                    running_mode=VisionRunningMode.VIDEO,
                    num_poses=1,
                    min_pose_detection_confidence=0.5,
                    min_pose_presence_confidence=0.5,
                    min_tracking_confidence=0.5,
                    output_segmentation_masks=False,
                )
                self.pose_landmarker = PoseLandmarker.create_from_options(options)
                logger.info("AvatarCharacter: MediaPipe PoseLandmarker initialized with GPU delegate")

            except Exception as e:
                logger.warning(f"AvatarCharacter: GPU delegate failed, trying CPU: {e}")
                try:
                    options = PoseLandmarkerOptions(
                        base_options=BaseOptions(
                            model_asset_path=str(POSE_MODEL_PATH),
                            delegate=BaseOptions.Delegate.CPU,
                        ),
                        running_mode=VisionRunningMode.VIDEO,
                        num_poses=1,
                        min_pose_detection_confidence=0.5,
                        min_pose_presence_confidence=0.5,
                        min_tracking_confidence=0.5,
                        output_segmentation_masks=False,
                    )
                    self.pose_landmarker = PoseLandmarker.create_from_options(options)
                    logger.info("AvatarCharacter: MediaPipe PoseLandmarker initialized with CPU delegate")
                except Exception as e2:
                    logger.error(f"AvatarCharacter: MediaPipe init failed completely: {e2}")
                    self.pose_landmarker = None
        else:
            logger.warning("AvatarCharacter: Running WITHOUT MediaPipe")

        # Get GL context from streamlib
        self.gl_ctx = ctx.gpu._experimental_gl_context()
        self.gl_ctx.make_current()

        # Create ModernGL context wrapping the existing OpenGL context
        self.moderngl_ctx = moderngl.create_context(standalone=False)
        logger.info(f"AvatarCharacter: ModernGL context created (GL {self.moderngl_ctx.version_code})")

        # Create texture bindings for input (pose detection readback) and output
        self.input_binding = self.gl_ctx.create_texture_binding()
        self.output_binding = self.gl_ctx.create_texture_binding()

        # GPU context for buffer allocation
        self._gpu_ctx = ctx.gpu
        self.output_buffer = None
        self._current_dims = None

        # 3D renderer (initialized on first frame when we know dimensions)
        self.renderer_3d = None
        self._render_fbo = None
        self._render_texture = None
        self._render_depth = None

        # Last valid pose (world landmarks for 3D)
        self.last_world_landmarks = None

        # Check if character model exists
        if not CHARACTER_PATH.exists():
            logger.error(f"AvatarCharacter: Character model not found at {CHARACTER_PATH}")
            raise FileNotFoundError(f"Character model not found: {CHARACTER_PATH}")

        logger.info("AvatarCharacter: Setup complete (3D mode)")

    def _init_renderer(self, width: int, height: int):
        """Initialize 3D renderer and FBO on first frame."""
        # Create 3D character renderer
        self.renderer_3d = CharacterRenderer3D(self.moderngl_ctx, width, height)

        # Load character model
        self.renderer_3d.load_character(CHARACTER_PATH)
        logger.info(f"AvatarCharacter: Loaded 3D character from {CHARACTER_PATH}")

        # Load background if available
        if BACKGROUND_PATH.exists():
            self.renderer_3d.load_background(BACKGROUND_PATH)
            logger.info(f"AvatarCharacter: Loaded background from {BACKGROUND_PATH}")

        # Create FBO for rendering
        self._render_texture = self.moderngl_ctx.texture((width, height), 4)
        self._render_depth = self.moderngl_ctx.depth_texture((width, height))
        self._render_fbo = self.moderngl_ctx.framebuffer(
            color_attachments=[self._render_texture],
            depth_attachment=self._render_depth,
        )

        # Record setup completion time for delayed slide-in
        self._setup_complete_time = time.monotonic()
        logger.info(f"AvatarCharacter: 3D renderer initialized ({width}x{height}) - sliding in after {self._ready_delay_seconds}s")

    def process(self, ctx):
        """Process frame: detect pose, render 3D character."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        self.gl_ctx.make_current()

        input_buffer = frame["pixel_buffer"]
        width = input_buffer.width
        height = input_buffer.height

        # Initialize renderer on first frame or resize
        if self._current_dims != (width, height):
            self._current_dims = (width, height)

            # Create output buffer and update binding
            self.output_buffer = self._gpu_ctx.acquire_pixel_buffer(
                width, height, PixelFormat.BGRA32
            )
            self.output_binding.update(self.output_buffer)

            # Initialize 3D renderer
            if self.renderer_3d is None:
                self._init_renderer(width, height)
            else:
                # Resize existing renderer
                self.renderer_3d.resize(width, height)

                # Recreate FBO at new size
                self._render_texture.release()
                self._render_depth.release()
                self._render_fbo.release()

                self._render_texture = self.moderngl_ctx.texture((width, height), 4)
                self._render_depth = self.moderngl_ctx.depth_texture((width, height))
                self._render_fbo = self.moderngl_ctx.framebuffer(
                    color_attachments=[self._render_texture],
                    depth_attachment=self._render_depth,
                )

        self.input_binding.update(input_buffer)

        # Detect pose using MediaPipe
        if self.pose_landmarker is not None:
            try:
                from OpenGL import GL

                if not hasattr(self, '_readback_fbo'):
                    self._readback_fbo = GL.glGenFramebuffers(1)

                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, self._readback_fbo)
                GL.glFramebufferTexture2D(
                    GL.GL_FRAMEBUFFER,
                    GL.GL_COLOR_ATTACHMENT0,
                    self.input_binding.target,
                    self.input_binding.id,
                    0
                )

                pixels = GL.glReadPixels(0, 0, width, height, GL.GL_RGBA, GL.GL_UNSIGNED_BYTE)
                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, 0)

                img_rgba = np.frombuffer(pixels, dtype=np.uint8).reshape(height, width, 4)
                img_rgba = np.flipud(img_rgba).copy()
                img_rgb = np.ascontiguousarray(img_rgba[:, :, :3])

                mp_image = mp.Image(image_format=mp.ImageFormat.SRGB, data=img_rgb)

                self._timestamp_ms += 33
                pose_result = self.pose_landmarker.detect_for_video(mp_image, self._timestamp_ms)

                # Use world landmarks for 3D pose (better depth information)
                if pose_result.pose_world_landmarks and len(pose_result.pose_world_landmarks) > 0:
                    self.last_world_landmarks = pose_result.pose_world_landmarks[0]

            except Exception as e:
                if self.frame_count % 60 == 0:
                    logger.warning(f"MediaPipe processing failed: {e}")

        # Update 3D character pose
        if self.last_world_landmarks is not None:
            self.renderer_3d.update_pose(self.last_world_landmarks)

        # Render 3D scene with bloom to FBO
        self.renderer_3d.render(output_fbo=self._render_fbo)

        # Read rendered pixels from FBO (RGBA)
        rendered_pixels = self._render_fbo.read(components=4)
        rendered_array = np.frombuffer(rendered_pixels, dtype=np.uint8).reshape(height, width, 4)

        # Convert RGBA to BGRA for output (swap R and B channels)
        rendered_bgra = rendered_array[:, :, [2, 1, 0, 3]].copy()

        # Upload to output texture via binding
        from OpenGL import GL
        GL.glBindTexture(self.output_binding.target, self.output_binding.id)
        GL.glTexSubImage2D(
            self.output_binding.target, 0, 0, 0, width, height,
            GL.GL_BGRA, GL.GL_UNSIGNED_BYTE, rendered_bgra.tobytes()
        )
        GL.glBindTexture(self.output_binding.target, 0)

        self.gl_ctx.flush()

        # Check if ready delay has passed (5 seconds after resources loaded)
        if not self._is_ready and self._setup_complete_time is not None:
            elapsed = time.monotonic() - self._setup_complete_time
            if elapsed >= self._ready_delay_seconds:
                self._is_ready = True
                logger.info(f"AvatarCharacter: Ready after {elapsed:.1f}s - triggering slide-in!")

        # Only output frames after the delay has passed
        # This triggers the slide-in animation in the compositor when first frame arrives
        if self._is_ready:
            ctx.output("video_out").set({
                "pixel_buffer": self.output_buffer,
                "timestamp_ns": frame["timestamp_ns"],
                "frame_number": frame["frame_number"],
                "pip_ready": True,
            })

        self.frame_count += 1
        if self.frame_count == 1:
            logger.info(f"AvatarCharacter: First frame processed ({width}x{height})")
        if self.frame_count % 300 == 0:
            logger.debug(f"AvatarCharacter: {self.frame_count} frames processed (ready={self._is_ready})")

    def teardown(self, ctx):
        """Cleanup resources."""
        if self.pose_landmarker is not None:
            self.pose_landmarker.close()

        if hasattr(self, '_readback_fbo') and self._readback_fbo is not None:
            try:
                from OpenGL import GL
                GL.glDeleteFramebuffers(1, [self._readback_fbo])
            except Exception:
                pass

        # Release ModernGL resources
        if self._render_fbo is not None:
            self._render_fbo.release()
        if self._render_texture is not None:
            self._render_texture.release()
        if self._render_depth is not None:
            self._render_depth.release()

        logger.info(f"AvatarCharacter: Shutdown ({self.frame_count} frames, ready={self._is_ready})")
