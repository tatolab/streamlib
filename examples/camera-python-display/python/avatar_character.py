# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Avatar Character Processor - Stylized cyberpunk character for PiP overlay.

Uses MediaPipe Tasks API (GPU-accelerated) for pose detection,
then renders a stylized geometric character on transparent background
for picture-in-picture overlay in the "Breaking News" style.

Features:
- MediaPipe PoseLandmarker with GPU/Metal delegate (33 landmarks)
- Stylized geometric character inspired by Cyberpunk Edgerunners
- Yellow jacket with cyan accents
- Transparent background for compositing
- "Ready" state signaling for slide-in animation timing
"""

import logging
import math
import os
import sys
import urllib.request
from pathlib import Path

import numpy as np
import skia

from streamlib import processor, input, output, PixelFormat

logger = logging.getLogger(__name__)

# Model configuration
MODEL_DIR = Path(__file__).parent / "models"
POSE_MODEL_PATH = MODEL_DIR / "pose_landmarker_lite.task"
POSE_MODEL_URL = "https://storage.googleapis.com/mediapipe-models/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task"

# Assets configuration
ASSETS_DIR = Path(__file__).parent.parent / "assets"
BACKGROUND_PATH = ASSETS_DIR / "alley.jpg"

# OpenGL constants
GL_RGBA8 = 0x8058


def ensure_model_downloaded():
    """Download pose landmarker model if not present."""
    MODEL_DIR.mkdir(parents=True, exist_ok=True)

    if not POSE_MODEL_PATH.exists():
        logger.info(f"Downloading pose model to {POSE_MODEL_PATH}...")
        try:
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


# MediaPipe landmark indices
class PoseLandmarkIndex:
    NOSE = 0
    LEFT_EYE_INNER = 1
    LEFT_EYE = 2
    LEFT_EYE_OUTER = 3
    RIGHT_EYE_INNER = 4
    RIGHT_EYE = 5
    RIGHT_EYE_OUTER = 6
    LEFT_EAR = 7
    RIGHT_EAR = 8
    MOUTH_LEFT = 9
    MOUTH_RIGHT = 10
    LEFT_SHOULDER = 11
    RIGHT_SHOULDER = 12
    LEFT_ELBOW = 13
    RIGHT_ELBOW = 14
    LEFT_WRIST = 15
    RIGHT_WRIST = 16
    LEFT_PINKY = 17
    RIGHT_PINKY = 18
    LEFT_INDEX = 19
    RIGHT_INDEX = 20
    LEFT_THUMB = 21
    RIGHT_THUMB = 22
    LEFT_HIP = 23
    RIGHT_HIP = 24
    LEFT_KNEE = 25
    RIGHT_KNEE = 26
    LEFT_ANKLE = 27
    RIGHT_ANKLE = 28
    LEFT_HEEL = 29
    RIGHT_HEEL = 30
    LEFT_FOOT_INDEX = 31
    RIGHT_FOOT_INDEX = 32


# =============================================================================
# Cyberpunk Color Palette
# =============================================================================

CYBER_YELLOW = (252, 238, 10, 255)       # #fcee0a - jacket
CYBER_YELLOW_DARK = (180, 160, 0, 255)   # Darker yellow for shading
CYBER_CYAN = (0, 240, 255, 255)          # #00f0ff - accents
CYBER_BLACK = (15, 15, 20, 255)          # Near black - shirt/pants
CYBER_GRAY = (60, 60, 70, 255)           # Gray for jeans
CYBER_SKIN = (220, 180, 160, 255)        # Skin tone
CYBER_HAIR = (80, 50, 30, 255)           # Brown hair
CYBER_HAIR_TIP = (200, 180, 50, 255)     # Yellow-tipped hair


def skia_color(rgba):
    """Convert RGBA tuple to Skia color."""
    return skia.Color(*rgba)


# =============================================================================
# Character Drawing Functions
# =============================================================================

def get_landmark_point(landmarks, idx, width, height, min_visibility=0.5, mirror_x=True):
    """Get 2D pixel coordinates for a landmark from Tasks API format.

    Args:
        mirror_x: Mirror X coordinate for front-facing camera (selfie mode)
    """
    if landmarks is None or idx >= len(landmarks):
        return None
    lm = landmarks[idx]
    visibility = getattr(lm, 'visibility', 1.0)
    if visibility is not None and visibility < min_visibility:
        return None
    # Mirror X for front-facing camera so character matches user movement
    x = (1.0 - lm.x) if mirror_x else lm.x
    return (int(x * width), int(lm.y * height))


def angle_between_points(p1, p2):
    """Get angle in radians from p1 to p2."""
    if p1 is None or p2 is None:
        return 0
    dx = p2[0] - p1[0]
    dy = p2[1] - p1[1]
    return math.atan2(dy, dx)


def distance_between_points(p1, p2):
    """Get distance between two points."""
    if p1 is None or p2 is None:
        return 0
    dx = p2[0] - p1[0]
    dy = p2[1] - p1[1]
    return math.sqrt(dx * dx + dy * dy)


def midpoint(p1, p2):
    """Get midpoint between two points."""
    if p1 is None or p2 is None:
        return None
    return ((p1[0] + p2[0]) // 2, (p1[1] + p2[1]) // 2)


def draw_angular_limb(canvas, p1, p2, width, color, outline_color=None):
    """Draw an angular/geometric limb segment."""
    if p1 is None or p2 is None:
        return

    angle = angle_between_points(p1, p2)
    perpendicular = angle + math.pi / 2
    half_width = width / 2

    dx = math.cos(perpendicular) * half_width
    dy = math.sin(perpendicular) * half_width

    path = skia.Path()
    path.moveTo(p1[0] - dx, p1[1] - dy)
    path.lineTo(p2[0] - dx * 0.8, p2[1] - dy * 0.8)
    path.lineTo(p2[0] + dx * 0.8, p2[1] + dy * 0.8)
    path.lineTo(p1[0] + dx, p1[1] + dy)
    path.close()

    paint = skia.Paint(Color=skia_color(color), AntiAlias=True)
    canvas.drawPath(path, paint)

    if outline_color:
        outline_paint = skia.Paint(
            Color=skia_color(outline_color),
            AntiAlias=True,
            Style=skia.Paint.kStroke_Style,
            StrokeWidth=2,
        )
        canvas.drawPath(path, outline_paint)


def draw_angular_joint(canvas, point, size, color, accent_color=None):
    """Draw an angular joint marker (diamond shape)."""
    if point is None:
        return

    x, y = point
    half = size / 2

    path = skia.Path()
    path.moveTo(x, y - half)
    path.lineTo(x + half, y)
    path.lineTo(x, y + half)
    path.lineTo(x - half, y)
    path.close()

    paint = skia.Paint(Color=skia_color(color), AntiAlias=True)
    canvas.drawPath(path, paint)

    if accent_color:
        inner_path = skia.Path()
        inner_half = half * 0.5
        inner_path.moveTo(x, y - inner_half)
        inner_path.lineTo(x + inner_half, y)
        inner_path.lineTo(x, y + inner_half)
        inner_path.lineTo(x - inner_half, y)
        inner_path.close()

        accent_paint = skia.Paint(Color=skia_color(accent_color), AntiAlias=True)
        canvas.drawPath(inner_path, accent_paint)


def draw_head(canvas, nose, left_ear, right_ear, shoulder_mid, scale):
    """Draw stylized angular head with spiky hair."""
    if nose is None:
        return

    head_center = (nose[0], nose[1] - int(40 * scale))
    head_size = int(50 * scale)
    cx, cy = head_center

    # Angular face shape
    path = skia.Path()
    path.moveTo(cx, cy - head_size)
    path.lineTo(cx + head_size * 0.7, cy - head_size * 0.5)
    path.lineTo(cx + head_size * 0.6, cy + head_size * 0.3)
    path.lineTo(cx + head_size * 0.3, cy + head_size * 0.7)
    path.lineTo(cx - head_size * 0.3, cy + head_size * 0.7)
    path.lineTo(cx - head_size * 0.6, cy + head_size * 0.3)
    path.lineTo(cx - head_size * 0.7, cy - head_size * 0.5)
    path.close()

    face_paint = skia.Paint(Color=skia_color(CYBER_SKIN), AntiAlias=True)
    canvas.drawPath(path, face_paint)

    outline_paint = skia.Paint(
        Color=skia_color(CYBER_BLACK),
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=2,
    )
    canvas.drawPath(path, outline_paint)

    # Spiky hair
    hair_path = skia.Path()
    hair_y = cy - head_size * 0.8
    hair_path.moveTo(cx - head_size * 0.8, hair_y + head_size * 0.3)

    spikes = [
        (cx - head_size * 0.5, hair_y - head_size * 0.3),
        (cx - head_size * 0.2, hair_y - head_size * 0.6),
        (cx + head_size * 0.1, hair_y - head_size * 0.4),
        (cx + head_size * 0.4, hair_y - head_size * 0.7),
        (cx + head_size * 0.6, hair_y - head_size * 0.3),
        (cx + head_size * 0.8, hair_y + head_size * 0.2),
    ]

    for spike in spikes:
        hair_path.lineTo(spike[0], spike[1])
    hair_path.lineTo(cx + head_size * 0.7, hair_y + head_size * 0.5)
    hair_path.close()

    hair_paint = skia.Paint(Color=skia_color(CYBER_HAIR), AntiAlias=True)
    canvas.drawPath(hair_path, hair_paint)

    # Yellow tips
    for i, spike in enumerate(spikes):
        if i % 2 == 0:
            tip_path = skia.Path()
            tip_size = head_size * 0.15
            tip_path.moveTo(spike[0], spike[1])
            tip_path.lineTo(spike[0] - tip_size, spike[1] + tip_size * 1.5)
            tip_path.lineTo(spike[0] + tip_size, spike[1] + tip_size * 1.5)
            tip_path.close()
            tip_paint = skia.Paint(Color=skia_color(CYBER_HAIR_TIP), AntiAlias=True)
            canvas.drawPath(tip_path, tip_paint)

    # Eyes
    eye_y = cy - head_size * 0.1
    eye_size = head_size * 0.12

    for eye_x_offset in [-head_size * 0.25, head_size * 0.25]:
        eye_x = cx + eye_x_offset
        eye_path = skia.Path()
        eye_path.moveTo(eye_x - eye_size, eye_y)
        eye_path.lineTo(eye_x, eye_y - eye_size * 0.5)
        eye_path.lineTo(eye_x + eye_size, eye_y)
        eye_path.lineTo(eye_x, eye_y + eye_size * 0.5)
        eye_path.close()
        eye_paint = skia.Paint(Color=skia_color(CYBER_BLACK), AntiAlias=True)
        canvas.drawPath(eye_path, eye_paint)


def draw_torso(canvas, left_shoulder, right_shoulder, left_hip, right_hip, scale):
    """Draw angular torso with yellow jacket."""
    if any(p is None for p in [left_shoulder, right_shoulder, left_hip, right_hip]):
        return

    jacket_path = skia.Path()
    expand = int(20 * scale)
    ls_outer = (left_shoulder[0] - expand, left_shoulder[1])
    rs_outer = (right_shoulder[0] + expand, right_shoulder[1])

    jacket_path.moveTo(ls_outer[0], ls_outer[1] - int(10 * scale))
    jacket_path.lineTo(rs_outer[0], rs_outer[1] - int(10 * scale))
    jacket_path.lineTo(rs_outer[0] + int(5 * scale), right_shoulder[1])
    jacket_path.lineTo(right_hip[0] + int(15 * scale), right_hip[1])
    jacket_path.lineTo(left_hip[0] - int(15 * scale), left_hip[1])
    jacket_path.lineTo(ls_outer[0] - int(5 * scale), left_shoulder[1])
    jacket_path.close()

    jacket_paint = skia.Paint(Color=skia_color(CYBER_YELLOW), AntiAlias=True)
    canvas.drawPath(jacket_path, jacket_paint)

    outline_paint = skia.Paint(
        Color=skia_color(CYBER_BLACK),
        AntiAlias=True,
        Style=skia.Paint.kStroke_Style,
        StrokeWidth=2,
    )
    canvas.drawPath(jacket_path, outline_paint)

    # Cyan stripes
    stripe_paint = skia.Paint(Color=skia_color(CYBER_CYAN), AntiAlias=True)

    left_stripe = skia.Path()
    left_stripe.moveTo(ls_outer[0] + int(15 * scale), ls_outer[1])
    left_stripe.lineTo(ls_outer[0] + int(20 * scale), ls_outer[1])
    left_stripe.lineTo(left_hip[0] - int(5 * scale), left_hip[1])
    left_stripe.lineTo(left_hip[0] - int(10 * scale), left_hip[1])
    left_stripe.close()
    canvas.drawPath(left_stripe, stripe_paint)

    right_stripe = skia.Path()
    right_stripe.moveTo(rs_outer[0] - int(15 * scale), rs_outer[1])
    right_stripe.lineTo(rs_outer[0] - int(20 * scale), rs_outer[1])
    right_stripe.lineTo(right_hip[0] + int(5 * scale), right_hip[1])
    right_stripe.lineTo(right_hip[0] + int(10 * scale), right_hip[1])
    right_stripe.close()
    canvas.drawPath(right_stripe, stripe_paint)

    # Black shirt (V-neck)
    shirt_path = skia.Path()
    neck_center = midpoint(left_shoulder, right_shoulder)
    if neck_center:
        shirt_path.moveTo(left_shoulder[0] + int(20 * scale), left_shoulder[1])
        shirt_path.lineTo(neck_center[0], neck_center[1] + int(60 * scale))
        shirt_path.lineTo(right_shoulder[0] - int(20 * scale), right_shoulder[1])
        shirt_path.close()
        shirt_paint = skia.Paint(Color=skia_color(CYBER_BLACK), AntiAlias=True)
        canvas.drawPath(shirt_path, shirt_paint)


def estimate_hips_from_shoulders(left_shoulder, right_shoulder, scale):
    """Estimate hip positions when not visible (upper body only in frame)."""
    if left_shoulder is None or right_shoulder is None:
        return None, None

    # Hips are typically ~1.2x shoulder width apart and ~1.5x shoulder width below
    shoulder_mid = midpoint(left_shoulder, right_shoulder)
    if shoulder_mid is None:
        return None, None

    shoulder_width = distance_between_points(left_shoulder, right_shoulder)
    hip_width = shoulder_width * 0.9  # Hips slightly narrower than shoulders
    torso_length = shoulder_width * 1.3  # Torso length estimate

    left_hip = (int(shoulder_mid[0] - hip_width / 2), int(shoulder_mid[1] + torso_length))
    right_hip = (int(shoulder_mid[0] + hip_width / 2), int(shoulder_mid[1] + torso_length))

    return left_hip, right_hip


def draw_character(canvas, landmarks, width, height):
    """Draw the full stylized character based on pose landmarks."""
    if landmarks is None or len(landmarks) == 0:
        return

    nose = get_landmark_point(landmarks, PoseLandmarkIndex.NOSE, width, height)
    left_ear = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_EAR, width, height)
    right_ear = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_EAR, width, height)
    left_shoulder = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_SHOULDER, width, height)
    right_shoulder = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_SHOULDER, width, height)
    left_elbow = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_ELBOW, width, height)
    right_elbow = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_ELBOW, width, height)
    left_wrist = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_WRIST, width, height)
    right_wrist = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_WRIST, width, height)

    # Try to get real hip positions, fall back to estimates
    left_hip = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_HIP, width, height, min_visibility=0.1)
    right_hip = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_HIP, width, height, min_visibility=0.1)

    shoulder_width = distance_between_points(left_shoulder, right_shoulder)
    scale = shoulder_width / 200.0 if shoulder_width > 0 else 1.0
    scale = max(0.5, min(2.0, scale))

    # Estimate hips if not detected (upper body only in frame)
    if left_hip is None or right_hip is None:
        left_hip, right_hip = estimate_hips_from_shoulders(left_shoulder, right_shoulder, scale)

    # Legs only if we have real hip data (not estimated)
    left_knee = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_KNEE, width, height, min_visibility=0.1)
    right_knee = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_KNEE, width, height, min_visibility=0.1)
    left_ankle = get_landmark_point(landmarks, PoseLandmarkIndex.LEFT_ANKLE, width, height, min_visibility=0.1)
    right_ankle = get_landmark_point(landmarks, PoseLandmarkIndex.RIGHT_ANKLE, width, height, min_visibility=0.1)

    limb_width = int(25 * scale)
    joint_size = int(15 * scale)

    # Legs (only if detected - won't draw if hips are estimated)
    if left_knee is not None:
        draw_angular_limb(canvas, left_hip, left_knee, limb_width, CYBER_GRAY, CYBER_BLACK)
        draw_angular_limb(canvas, left_knee, left_ankle, limb_width * 0.9, CYBER_GRAY, CYBER_BLACK)
        draw_angular_joint(canvas, left_knee, joint_size, CYBER_BLACK, CYBER_CYAN)
    if right_knee is not None:
        draw_angular_limb(canvas, right_hip, right_knee, limb_width, CYBER_GRAY, CYBER_BLACK)
        draw_angular_limb(canvas, right_knee, right_ankle, limb_width * 0.9, CYBER_GRAY, CYBER_BLACK)
        draw_angular_joint(canvas, right_knee, joint_size, CYBER_BLACK, CYBER_CYAN)

    # Torso (works with estimated hips too)
    draw_torso(canvas, left_shoulder, right_shoulder, left_hip, right_hip, scale)

    # Arms
    draw_angular_limb(canvas, left_shoulder, left_elbow, limb_width, CYBER_YELLOW, CYBER_BLACK)
    draw_angular_limb(canvas, left_elbow, left_wrist, limb_width * 0.85, CYBER_YELLOW, CYBER_BLACK)
    draw_angular_limb(canvas, right_shoulder, right_elbow, limb_width, CYBER_YELLOW, CYBER_BLACK)
    draw_angular_limb(canvas, right_elbow, right_wrist, limb_width * 0.85, CYBER_YELLOW, CYBER_BLACK)

    draw_angular_joint(canvas, left_elbow, joint_size, CYBER_YELLOW_DARK, CYBER_CYAN)
    draw_angular_joint(canvas, right_elbow, joint_size, CYBER_YELLOW_DARK, CYBER_CYAN)

    draw_angular_joint(canvas, left_wrist, joint_size * 0.8, CYBER_SKIN, None)
    draw_angular_joint(canvas, right_wrist, joint_size * 0.8, CYBER_SKIN, None)

    # Head
    shoulder_mid = midpoint(left_shoulder, right_shoulder)
    draw_head(canvas, nose, left_ear, right_ear, shoulder_mid, scale)


# =============================================================================
# Avatar Character Processor (PiP Mode - Transparent Background)
# =============================================================================

@processor(
    name="AvatarCharacter",
    description="Pose-tracking cyberpunk character for PiP overlay",
)
class AvatarCharacter:
    """Renders a stylized character on transparent background for PiP overlay.

    Uses MediaPipe Tasks API with GPU delegate for pose detection.
    Outputs frames with transparent background - only the character is visible.
    Signals "ready" state when first pose is detected (for slide-in animation).
    """

    @input(schema="VideoFrame")
    def video_in(self):
        pass

    @output(schema="VideoFrame")
    def video_out(self):
        pass

    def setup(self, ctx):
        """Initialize MediaPipe Tasks API and Skia resources."""
        self.frame_count = 0
        self.pose_landmarker = None
        self._timestamp_ms = 0
        self._mediapipe_available = MEDIAPIPE_AVAILABLE

        # Ready state - becomes True when first pose is detected
        self._is_ready = False
        self._ready_frame_count = 0

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
                    output_segmentation_masks=False,  # Don't need segmentation for PiP
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

        # Get GL context
        self.gl_ctx = ctx.gpu._experimental_gl_context()
        self.gl_ctx.make_current()

        # Create Skia GPU context
        self.skia_ctx = skia.GrDirectContext.MakeGL()
        if self.skia_ctx is None:
            raise RuntimeError("Failed to create Skia GL context")

        # Create texture bindings
        self.input_binding = self.gl_ctx.create_texture_binding()
        self.output_binding = self.gl_ctx.create_texture_binding()

        # GPU context for buffer allocation
        self._gpu_ctx = ctx.gpu
        self.output_buffer = None
        self._current_dims = None
        self.skia_surface = None

        # Last valid pose
        self.last_landmarks = None

        # Load background image
        self.background_image = None
        if BACKGROUND_PATH.exists():
            try:
                self.background_image = skia.Image.open(str(BACKGROUND_PATH))
                logger.info(f"AvatarCharacter: Loaded background ({self.background_image.width()}x{self.background_image.height()})")
            except Exception as e:
                logger.warning(f"AvatarCharacter: Failed to load background: {e}")
        else:
            logger.warning(f"AvatarCharacter: Background not found at {BACKGROUND_PATH}")

        logger.info("AvatarCharacter: Setup complete (PiP mode)")

    def process(self, ctx):
        """Process frame: detect pose, render character on transparent background."""
        frame = ctx.input("video_in").get()
        if frame is None:
            return

        self.gl_ctx.make_current()

        input_buffer = frame["pixel_buffer"]
        width = input_buffer.width
        height = input_buffer.height

        # Ensure output buffer exists
        if self._current_dims != (width, height):
            self.output_buffer = self._gpu_ctx.acquire_pixel_buffer(
                width, height, PixelFormat.BGRA32
            )
            self._current_dims = (width, height)
            self.output_binding.update(self.output_buffer)

            output_gl_info = skia.GrGLTextureInfo(
                self.output_binding.target, self.output_binding.id, GL_RGBA8
            )
            output_backend = skia.GrBackendTexture(
                width, height, skia.GrMipmapped.kNo, output_gl_info
            )
            self.skia_surface = skia.Surface.MakeFromBackendTexture(
                self.skia_ctx,
                output_backend,
                skia.GrSurfaceOrigin.kTopLeft_GrSurfaceOrigin,
                0,
                skia.ColorType.kBGRA_8888_ColorType,
                None,
                None,
            )

        self.input_binding.update(input_buffer)

        # Detect pose
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

                if pose_result.pose_landmarks and len(pose_result.pose_landmarks) > 0:
                    self.last_landmarks = pose_result.pose_landmarks[0]

                    # First pose detected - we're ready!
                    if not self._is_ready:
                        self._is_ready = True
                        self._ready_frame_count = self.frame_count
                        logger.info("AvatarCharacter: First pose detected - READY for display!")

            except Exception as e:
                if self.frame_count % 60 == 0:
                    logger.warning(f"MediaPipe processing failed: {e}")

        # Get canvas and draw background (or clear to transparent if no background)
        canvas = self.skia_surface.getCanvas()
        if self.background_image is not None:
            # Scale background to fill the frame
            src_rect = skia.Rect.MakeWH(self.background_image.width(), self.background_image.height())
            dst_rect = skia.Rect.MakeWH(width, height)
            canvas.drawImageRect(self.background_image, src_rect, dst_rect)
        else:
            canvas.clear(skia.ColorTRANSPARENT)

        # Draw character ONLY if we have pose
        if self.last_landmarks:
            draw_character(canvas, self.last_landmarks, width, height)

        self.skia_surface.flushAndSubmit()
        self.gl_ctx.flush()

        # Output frame with metadata
        # Include "ready" flag so compositor knows when to show PiP
        ctx.output("video_out").set({
            "pixel_buffer": self.output_buffer,
            "timestamp_ns": frame["timestamp_ns"],
            "frame_number": frame["frame_number"],
            "pip_ready": self._is_ready,  # Signal for slide-in animation
        })

        self.frame_count += 1
        if self.frame_count == 1:
            logger.info(f"AvatarCharacter: First frame processed ({width}x{height})")
        if self.frame_count % 300 == 0:
            self.skia_ctx.freeGpuResources()
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

        if self.skia_ctx:
            self.skia_ctx.abandonContext()

        logger.info(f"AvatarCharacter: Shutdown ({self.frame_count} frames, ready={self._is_ready})")
