# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Avatar Character Processor - 3D cyberpunk character for PiP overlay.

Isolated subprocess processor using standalone CGL context.
Uses MediaPipe Tasks API for pose detection, then renders a 3D rigged
character driven by pose landmarks. The character is displayed in a
picture-in-picture overlay with a "Breaking News" style.

Features:
- MediaPipe PoseLandmarker with GPU/CPU delegate (33 3D landmarks)
- 3D rigged character from GLB file (Mixamo skeleton)
- GPU-accelerated skinned mesh rendering via ModernGL
- Real-time pose-to-bone rotation conversion
- Optional 3D scene background
- "Ready" state signaling for slide-in animation timing
- Zero-copy GPU texture via IOSurface + CGL binding
"""

import logging
import sys
import time
from pathlib import Path

logger = logging.getLogger(__name__)

# VideoFrame msgpack array indices
FRAME_INDEX = 0
HEIGHT = 1
SURFACE_ID = 2
TIMESTAMP_NS = 3
WIDTH = 4

# Delay heavy imports to avoid GIL deadlock during parallel module loading
moderngl = None
np = None
CharacterRenderer3D = None

def _lazy_import():
    """Import heavy dependencies lazily to avoid import-time GIL issues."""
    global moderngl, np, CharacterRenderer3D
    global MEDIAPIPE_AVAILABLE, mp, PoseLandmarker, PoseLandmarkerOptions, BaseOptions, VisionRunningMode
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

    if not MEDIAPIPE_AVAILABLE and mp is None:
        try:
            import mediapipe as _mp
            from mediapipe.tasks import python as mp_tasks
            from mediapipe.tasks.python import vision
            from mediapipe.tasks.python.vision import (
                PoseLandmarker as _PL,
                PoseLandmarkerOptions as _PLO,
            )

            mp = _mp
            PoseLandmarker = _PL
            PoseLandmarkerOptions = _PLO
            BaseOptions = mp_tasks.BaseOptions
            VisionRunningMode = vision.RunningMode
            MEDIAPIPE_AVAILABLE = True
            logger.info(f"MediaPipe Tasks API loaded (version: {mp.__version__})")
        except ImportError as e:
            logger.warning(f"MediaPipe Tasks API import failed: {e}")
        except Exception as e:
            logger.warning(f"MediaPipe initialization error: {e}")


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


# MediaPipe globals - imported lazily in _lazy_import() to avoid blocking subprocess startup
MEDIAPIPE_AVAILABLE = False
mp = None
PoseLandmarker = None
PoseLandmarkerOptions = None
BaseOptions = None
VisionRunningMode = None


# =============================================================================
# Avatar Character Processor (Isolated Subprocess - 3D Rendering Mode)
# =============================================================================

class AvatarCharacter:
    """Renders a 3D rigged character driven by MediaPipe pose landmarks.

    Isolated subprocess processor with own CGL context.
    Uses MediaPipe Tasks API for pose detection.
    Renders a 3D character model with GPU skinning via ModernGL.
    Signals readiness by delaying output until first pose is stable.
    """

    def setup(self, ctx):
        """Initialize standalone CGL context, MediaPipe, and 3D rendering."""
        # Lazy import heavy dependencies
        _lazy_import()

        from streamlib.cgl_context import create_cgl_context, make_current

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

        # Create standalone CGL context (own GPU context, not host's)
        self.cgl_ctx = create_cgl_context()
        make_current(self.cgl_ctx)

        # Create ModernGL context wrapping our CGL context
        self.moderngl_ctx = moderngl.create_context(standalone=False)
        logger.info(f"AvatarCharacter: ModernGL context created (GL {self.moderngl_ctx.version_code})")

        # Create GL textures for input readback and output IOSurface binding
        from OpenGL.GL import glGenTextures, glGenFramebuffers
        self.input_tex_id = glGenTextures(1)
        self.output_tex_id = glGenTextures(1)
        self._readback_fbo = glGenFramebuffers(1)

        # Track current dimensions
        self._current_dims = None
        self.output_surface_id = None

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

        logger.info("AvatarCharacter: Setup complete (3D mode, standalone CGL context)")

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
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        from streamlib.cgl_context import make_current, bind_iosurface_to_texture, flush
        from OpenGL import GL

        make_current(self.cgl_ctx)

        w = frame[WIDTH]
        h = frame[HEIGHT]

        # Initialize renderer on first frame or resize
        if self._current_dims != (w, h):
            self._current_dims = (w, h)

            # Initialize 3D renderer
            if self.renderer_3d is None:
                self._init_renderer(w, h)
            else:
                # Resize existing renderer
                self.renderer_3d.resize(w, h)

                # Recreate FBO at new size
                self._render_texture.release()
                self._render_depth.release()
                self._render_fbo.release()

                self._render_texture = self.moderngl_ctx.texture((w, h), 4)
                self._render_depth = self.moderngl_ctx.depth_texture((w, h))
                self._render_fbo = self.moderngl_ctx.framebuffer(
                    color_attachments=[self._render_texture],
                    depth_attachment=self._render_depth,
                )

        # Resolve input surface → bind as GL texture for readback
        input_handle = ctx.gpu.resolve_surface(frame[SURFACE_ID])
        bind_iosurface_to_texture(
            self.cgl_ctx, self.input_tex_id,
            input_handle.iosurface_ref, w, h
        )

        # Detect pose using MediaPipe (requires CPU readback from GPU texture)
        if self.pose_landmarker is not None:
            try:
                from streamlib.cgl_context import GL_TEXTURE_RECTANGLE

                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, self._readback_fbo)
                GL.glFramebufferTexture2D(
                    GL.GL_FRAMEBUFFER,
                    GL.GL_COLOR_ATTACHMENT0,
                    GL_TEXTURE_RECTANGLE,
                    self.input_tex_id,
                    0
                )

                pixels = GL.glReadPixels(0, 0, w, h, GL.GL_RGBA, GL.GL_UNSIGNED_BYTE)
                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, 0)

                img_rgba = np.frombuffer(pixels, dtype=np.uint8).reshape(h, w, 4)
                img_rgba = np.flipud(img_rgba).copy()

                mp_image = mp.Image(image_format=mp.ImageFormat.SRGBA, data=img_rgba)

                self._timestamp_ms += 33
                pose_result = self.pose_landmarker.detect_for_video(mp_image, self._timestamp_ms)

                # Use world landmarks for 3D pose (better depth information)
                if pose_result.pose_world_landmarks and len(pose_result.pose_world_landmarks) > 0:
                    self.last_world_landmarks = pose_result.pose_world_landmarks[0]

            except Exception as e:
                if self.frame_count % 60 == 0:
                    logger.warning(f"MediaPipe processing failed: {e}")

        input_handle.release()

        # Update 3D character pose
        if self.last_world_landmarks is not None:
            self.renderer_3d.update_pose(self.last_world_landmarks)

        # Render 3D scene with bloom to FBO
        self.renderer_3d.render(output_fbo=self._render_fbo)

        # Read rendered pixels from FBO (RGBA)
        rendered_pixels = self._render_fbo.read(components=4)
        rendered_array = np.frombuffer(rendered_pixels, dtype=np.uint8).reshape(h, w, 4)

        # Convert RGBA to BGRA for output (swap R and B channels)
        rendered_bgra = rendered_array[:, :, [2, 1, 0, 3]].copy()

        # Acquire output surface → bind as GL texture → upload rendered pixels
        out_surface_id, output_handle = ctx.gpu.acquire_surface(width=w, height=h, format="bgra")
        bind_iosurface_to_texture(
            self.cgl_ctx, self.output_tex_id,
            output_handle.iosurface_ref, w, h
        )

        from streamlib.cgl_context import GL_TEXTURE_RECTANGLE
        GL.glBindTexture(GL_TEXTURE_RECTANGLE, self.output_tex_id)
        GL.glTexSubImage2D(
            GL_TEXTURE_RECTANGLE, 0, 0, 0, w, h,
            GL.GL_BGRA, GL.GL_UNSIGNED_BYTE, rendered_bgra.tobytes()
        )
        GL.glBindTexture(GL_TEXTURE_RECTANGLE, 0)

        flush()
        output_handle.release()

        # Check if ready delay has passed (1.5 seconds after resources loaded)
        if not self._is_ready and self._setup_complete_time is not None:
            elapsed = time.monotonic() - self._setup_complete_time
            if elapsed >= self._ready_delay_seconds:
                self._is_ready = True
                logger.info(f"AvatarCharacter: Ready after {elapsed:.1f}s - triggering slide-in!")

        # Only output frames after the delay has passed
        # This triggers the slide-in animation in the compositor when first frame arrives
        if self._is_ready:
            out_frame = list(frame)
            out_frame[SURFACE_ID] = out_surface_id
            ctx.outputs.write("video_out", out_frame)

        self.frame_count += 1
        if self.frame_count == 1:
            logger.info(f"AvatarCharacter: First frame processed ({w}x{h})")
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

        if hasattr(self, 'cgl_ctx'):
            from streamlib.cgl_context import destroy_cgl_context
            destroy_cgl_context(self.cgl_ctx)

        logger.info(f"AvatarCharacter: Shutdown ({self.frame_count} frames, ready={self._is_ready})")
