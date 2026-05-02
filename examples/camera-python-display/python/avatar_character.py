# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Avatar Character Processor — pose-driven cyberpunk overlay.

Isolated subprocess processor that runs pose detection on the input camera
frame and renders a TikTok-filter-style overlay driven by the detected
pose.

Two platform-specific paths:

- **macOS** (legacy): MediaPipe Tasks pose detection + GLB-skinned 3D
  character via `CharacterRenderer3D` rendered to an IOSurface. Standalone
  CGL context + IOSurface zero-copy texture binding.

- **Linux** (#484): camera bytes → `streamlib-adapter-cuda` HOST_VISIBLE
  OPAQUE_FD `VkBuffer` → `torch.from_dlpack` zero-copy → Ultralytics
  YOLOv8n-pose. The camera frame is uploaded as a ModernGL background
  texture; a neon-cyan skeleton + magenta joint dots are rendered over it
  via `PoseOverlayRenderer` into the `streamlib-adapter-opengl` DMA-BUF
  surface (no skinned mesh, no GLB asset gating). Output is the
  pre-registered surface UUID.

Linux config keys (set by `examples/camera-python-display/src/linux.rs`):
    cuda_camera_surface_id (int)   — pre-registered cuda OPAQUE_FD buffer.
    opengl_output_surface_uuid (str) — pre-registered opengl DMA-BUF VkImage.
    width, height, channels (int)  — surface dimensions.
"""

import ctypes
import logging
import sys
import time
from pathlib import Path

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess

logger = logging.getLogger(__name__)

# Lazy globals — populated by the platform-specific lazy-import helpers.
moderngl = None
np = None
CharacterRenderer3D = None
PoseOverlayRenderer = None

# macOS pose-detection globals.
MEDIAPIPE_AVAILABLE = False
mp = None
PoseLandmarker = None
PoseLandmarkerOptions = None
BaseOptions = None
VisionRunningMode = None

# Linux pose-detection globals.
torch = None
cv2 = None
YOLO = None


def _lazy_import_common():
    """ModernGL + NumPy. Platform-specific renderer modules are imported
    in their own setup helpers — Linux doesn't need GLB / pyrr; macOS
    doesn't need ultralytics."""
    global moderngl, np
    if moderngl is None:
        import moderngl as _moderngl
        import numpy as _np
        moderngl = _moderngl
        np = _np

        _module_dir = Path(__file__).parent
        if str(_module_dir) not in sys.path:
            sys.path.insert(0, str(_module_dir))


def _lazy_import_macos_renderer():
    """The macOS path's GLB-skinning renderer."""
    global CharacterRenderer3D
    if CharacterRenderer3D is None:
        from character_renderer_3d import CharacterRenderer3D as _CR3D
        CharacterRenderer3D = _CR3D


def _lazy_import_linux_renderer():
    """The Linux path's lightweight pose-overlay renderer."""
    global PoseOverlayRenderer
    if PoseOverlayRenderer is None:
        from pose_overlay_renderer import PoseOverlayRenderer as _POR
        PoseOverlayRenderer = _POR


def _lazy_import_macos():
    """MediaPipe Tasks API (macOS path)."""
    global MEDIAPIPE_AVAILABLE, mp, PoseLandmarker, PoseLandmarkerOptions
    global BaseOptions, VisionRunningMode
    if MEDIAPIPE_AVAILABLE or mp is not None:
        return
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


def _lazy_import_linux():
    """PyTorch + Ultralytics + cv2 (Linux path)."""
    global torch, cv2, YOLO
    if torch is not None:
        return
    import torch as _torch  # CUDA-capable wheel required at install time
    import cv2 as _cv2
    from ultralytics import YOLO as _YOLO
    torch = _torch
    cv2 = _cv2
    YOLO = _YOLO


# Pose-model + assets configuration.
MODEL_DIR = Path(__file__).parent / "models"
POSE_MODEL_PATH = MODEL_DIR / "pose_landmarker_lite.task"
POSE_MODEL_URL = (
    "https://storage.googleapis.com/mediapipe-models/pose_landmarker/"
    "pose_landmarker_lite/float16/1/pose_landmarker_lite.task"
)

ASSETS_DIR = Path(__file__).parent.parent / "assets"
BACKGROUND_PATH = ASSETS_DIR / "alley.jpg"
CHARACTER_PATH = ASSETS_DIR / "character" / "character.glb"


def ensure_mediapipe_model_downloaded():
    """Download MediaPipe pose landmarker model if not present (macOS path)."""
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    if not POSE_MODEL_PATH.exists():
        logger.info(f"Downloading pose model to {POSE_MODEL_PATH}...")
        try:
            import urllib.request
            urllib.request.urlretrieve(POSE_MODEL_URL, POSE_MODEL_PATH)
            logger.info(
                f"Downloaded pose model "
                f"({POSE_MODEL_PATH.stat().st_size / 1024 / 1024:.1f} MB)"
            )
        except Exception as e:
            logger.error(f"Failed to download pose model: {e}")
            return False
    return POSE_MODEL_PATH.exists()


# =============================================================================
# Avatar Character Processor
# =============================================================================

class AvatarCharacter:
    """Runs pose detection on the input camera frame and renders a 3D
    rigged character driven by the detected pose."""

    # ---- Lifecycle dispatch ----------------------------------------------

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _lazy_import_common()
        if sys.platform == "linux":
            _lazy_import_linux_renderer()
            self._setup_linux(ctx)
        elif sys.platform == "darwin":
            _lazy_import_macos_renderer()
            self._setup_macos(ctx)
        else:
            raise RuntimeError(
                f"AvatarCharacter: unsupported platform {sys.platform!r} — "
                "only macOS and Linux are wired."
            )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if sys.platform == "linux":
            self._process_linux(ctx)
        elif sys.platform == "darwin":
            self._process_macos(ctx)
        else:
            raise RuntimeError(
                f"AvatarCharacter: unsupported platform {sys.platform!r}"
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        if sys.platform == "linux":
            self._teardown_linux(ctx)
        elif sys.platform == "darwin":
            self._teardown_macos(ctx)
        else:
            raise RuntimeError(
                f"AvatarCharacter: unsupported platform {sys.platform!r}"
            )

    # =====================================================================
    # macOS path (CGL + IOSurface + MediaPipe Tasks)
    # =====================================================================

    def _setup_macos(self, ctx: RuntimeContextFullAccess) -> None:
        _lazy_import_macos()
        from streamlib.cgl_context import create_cgl_context, make_current

        self.frame_count = 0
        self.pose_landmarker = None
        self._timestamp_ms = 0
        self._mediapipe_available = MEDIAPIPE_AVAILABLE

        self._is_ready = False
        self._setup_complete_time = None
        self._ready_delay_seconds = 1.5

        if self._mediapipe_available:
            if not ensure_mediapipe_model_downloaded():
                logger.error("AvatarCharacter: Failed to download pose model")
                self._mediapipe_available = False

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
                logger.info("AvatarCharacter: MediaPipe PoseLandmarker (GPU)")
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
                    logger.info("AvatarCharacter: MediaPipe PoseLandmarker (CPU)")
                except Exception as e2:
                    logger.error(f"AvatarCharacter: MediaPipe init failed: {e2}")
                    self.pose_landmarker = None

        self.cgl_ctx = create_cgl_context()
        make_current(self.cgl_ctx)

        self.moderngl_ctx = moderngl.create_context(standalone=False)
        logger.info(
            f"AvatarCharacter: ModernGL context created "
            f"(GL {self.moderngl_ctx.version_code})"
        )

        from OpenGL.GL import glGenTextures, glGenFramebuffers
        self.input_tex_id = glGenTextures(1)
        self.output_tex_id = glGenTextures(1)
        self._readback_fbo = glGenFramebuffers(1)

        self._current_dims = None
        self.output_surface_id = None

        self.renderer_3d = None
        self._render_fbo = None
        self._render_texture = None
        self._render_depth = None

        self.last_world_landmarks = None

        if not CHARACTER_PATH.exists():
            logger.warning(
                f"AvatarCharacter: Character model not found at {CHARACTER_PATH} "
                "— rendering will produce a background-only or solid-clear "
                "frame. See assets/README.md for the Mixamo download steps."
            )

        logger.info("AvatarCharacter: Setup complete (macOS path)")

    def _macos_init_renderer(self, width: int, height: int):
        self.renderer_3d = CharacterRenderer3D(self.moderngl_ctx, width, height)
        if CHARACTER_PATH.exists():
            try:
                self.renderer_3d.load_character(CHARACTER_PATH)
                logger.info(
                    f"AvatarCharacter: Loaded 3D character from {CHARACTER_PATH}"
                )
            except Exception as e:
                logger.warning(
                    f"AvatarCharacter: character load failed: {e} — "
                    "falling back to background-only render"
                )

        if BACKGROUND_PATH.exists():
            self.renderer_3d.load_background(BACKGROUND_PATH)
            logger.info(f"AvatarCharacter: Loaded background from {BACKGROUND_PATH}")

        self._render_texture = self.moderngl_ctx.texture((width, height), 4)
        self._render_depth = self.moderngl_ctx.depth_texture((width, height))
        self._render_fbo = self.moderngl_ctx.framebuffer(
            color_attachments=[self._render_texture],
            depth_attachment=self._render_depth,
        )
        self._setup_complete_time = time.monotonic()
        logger.info(
            f"AvatarCharacter: 3D renderer initialized ({width}x{height}) — "
            f"sliding in after {self._ready_delay_seconds}s"
        )

    def _process_macos(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        from streamlib.cgl_context import make_current, bind_iosurface_to_texture, flush
        from OpenGL import GL

        make_current(self.cgl_ctx)

        w = frame["width"]
        h = frame["height"]

        if self._current_dims != (w, h):
            self._current_dims = (w, h)
            if self.renderer_3d is None:
                self._macos_init_renderer(w, h)
            else:
                self.renderer_3d.resize(w, h)
                self._render_texture.release()
                self._render_depth.release()
                self._render_fbo.release()
                self._render_texture = self.moderngl_ctx.texture((w, h), 4)
                self._render_depth = self.moderngl_ctx.depth_texture((w, h))
                self._render_fbo = self.moderngl_ctx.framebuffer(
                    color_attachments=[self._render_texture],
                    depth_attachment=self._render_depth,
                )

        input_handle = ctx.gpu_limited_access.resolve_surface(frame["surface_id"])
        bind_iosurface_to_texture(
            self.cgl_ctx, self.input_tex_id, input_handle.iosurface_ref, w, h
        )

        if self.pose_landmarker is not None:
            try:
                from streamlib.cgl_context import GL_TEXTURE_RECTANGLE
                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, self._readback_fbo)
                GL.glFramebufferTexture2D(
                    GL.GL_FRAMEBUFFER,
                    GL.GL_COLOR_ATTACHMENT0,
                    GL_TEXTURE_RECTANGLE,
                    self.input_tex_id,
                    0,
                )
                pixels = GL.glReadPixels(0, 0, w, h, GL.GL_RGBA, GL.GL_UNSIGNED_BYTE)
                GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, 0)
                img_rgba = np.frombuffer(pixels, dtype=np.uint8).reshape(h, w, 4)
                img_rgba = np.flipud(img_rgba).copy()
                mp_image = mp.Image(image_format=mp.ImageFormat.SRGBA, data=img_rgba)
                self._timestamp_ms += 33
                pose_result = self.pose_landmarker.detect_for_video(
                    mp_image, self._timestamp_ms
                )
                if pose_result.pose_world_landmarks and len(pose_result.pose_world_landmarks) > 0:
                    self.last_world_landmarks = pose_result.pose_world_landmarks[0]
            except Exception as e:
                if self.frame_count % 60 == 0:
                    logger.warning(f"MediaPipe processing failed: {e}")

        input_handle.release()

        if self.last_world_landmarks is not None:
            self.renderer_3d.update_pose(self.last_world_landmarks)

        self.renderer_3d.render(output_fbo=self._render_fbo)

        rendered_pixels = self._render_fbo.read(components=4)
        rendered_array = np.frombuffer(rendered_pixels, dtype=np.uint8).reshape(h, w, 4)
        rendered_bgra = rendered_array[:, :, [2, 1, 0, 3]].copy()

        # TODO(#325/#369): replace `acquire_surface` with the future
        # escalate `acquire_texture` op once macOS grows the polyglot path.
        out_surface_id, output_handle = ctx.gpu_full_access.acquire_surface(  # type: ignore[attr-defined]
            width=w, height=h, format="bgra",
        )
        bind_iosurface_to_texture(
            self.cgl_ctx, self.output_tex_id, output_handle.iosurface_ref, w, h
        )

        from streamlib.cgl_context import GL_TEXTURE_RECTANGLE
        GL.glBindTexture(GL_TEXTURE_RECTANGLE, self.output_tex_id)
        GL.glTexSubImage2D(
            GL_TEXTURE_RECTANGLE, 0, 0, 0, w, h,
            GL.GL_BGRA, GL.GL_UNSIGNED_BYTE, rendered_bgra.tobytes(),
        )
        GL.glBindTexture(GL_TEXTURE_RECTANGLE, 0)

        flush()
        output_handle.release()

        if not self._is_ready and self._setup_complete_time is not None:
            elapsed = time.monotonic() - self._setup_complete_time
            if elapsed >= self._ready_delay_seconds:
                self._is_ready = True
                logger.info(
                    f"AvatarCharacter: Ready after {elapsed:.1f}s — slide-in!"
                )

        if self._is_ready:
            out_frame = dict(frame)
            out_frame["surface_id"] = out_surface_id
            ctx.outputs.write("video_out", out_frame)

        self.frame_count += 1
        if self.frame_count == 1:
            logger.info(f"AvatarCharacter: First frame processed ({w}x{h})")
        if self.frame_count % 300 == 0:
            logger.debug(
                f"AvatarCharacter: {self.frame_count} frames "
                f"(ready={self._is_ready})"
            )

    def _teardown_macos(self, ctx: RuntimeContextFullAccess) -> None:
        if self.pose_landmarker is not None:
            self.pose_landmarker.close()
        if hasattr(self, '_readback_fbo') and self._readback_fbo is not None:
            try:
                from OpenGL import GL
                GL.glDeleteFramebuffers(1, [self._readback_fbo])
            except Exception:
                pass
        if self._render_fbo is not None:
            self._render_fbo.release()
        if self._render_texture is not None:
            self._render_texture.release()
        if self._render_depth is not None:
            self._render_depth.release()
        if hasattr(self, 'cgl_ctx'):
            from streamlib.cgl_context import destroy_cgl_context
            destroy_cgl_context(self.cgl_ctx)
        logger.info(
            f"AvatarCharacter: Shutdown ({self.frame_count} frames, "
            f"ready={self._is_ready})"
        )

    # =====================================================================
    # Linux path (cuda OPAQUE_FD + opengl DMA-BUF + cyberpunk pose overlay)
    # =====================================================================

    def _setup_linux(self, ctx: RuntimeContextFullAccess) -> None:
        _lazy_import_linux()
        from streamlib.adapters.cuda import CudaContext
        from streamlib.adapters.opengl import OpenGLContext

        cfg = ctx.config
        self._cuda_camera_id = int(cfg["cuda_camera_surface_id"])
        self._opengl_uuid = str(cfg["opengl_output_surface_uuid"])
        self._W = int(cfg["width"])
        self._H = int(cfg["height"])
        self._channels = int(cfg["channels"])
        if self._channels != 4:
            raise ValueError(
                f"AvatarCharacter/linux: expected channels=4 (BGRA), got "
                f"channels={self._channels}"
            )

        self.frame_count = 0
        self._last_keypoints = None  # (17, 3) numpy array (x_px, y_px, conf)

        self._cuda = CudaContext.from_runtime(ctx)
        self._opengl = OpenGLContext.from_runtime(ctx)

        if not torch.cuda.is_available():
            raise RuntimeError(
                "AvatarCharacter/linux: torch.cuda.is_available() == False — "
                "install the CUDA-capable PyTorch wheel for your CUDA "
                "version (e.g. torch+cu121)."
            )
        device_name = torch.cuda.get_device_name(0)
        logger.info(
            f"AvatarCharacter/linux: torch {torch.__version__} on "
            f"cuda:0 ({device_name})"
        )

        # Pin YOLO weights to ~/.cache/ultralytics-streamlib so Ultralytics'
        # default cwd-relative download doesn't litter the repo root with
        # `yolov8n-pose.pt` on first run. Pre-fetch via urllib if absent;
        # then load by absolute path so YOLO sees an existing file and
        # skips its own download path entirely.
        weights_dir = Path.home() / ".cache" / "ultralytics-streamlib"
        weights_dir.mkdir(parents=True, exist_ok=True)
        weights_path = weights_dir / "yolov8n-pose.pt"
        if not weights_path.exists():
            import urllib.request
            url = (
                "https://github.com/ultralytics/assets/releases/"
                "download/v8.3.0/yolov8n-pose.pt"
            )
            logger.info(
                f"AvatarCharacter/linux: downloading YOLOv8n-pose weights "
                f"to {weights_path}..."
            )
            urllib.request.urlretrieve(url, weights_path)

        load_t0 = time.perf_counter()
        self._pose_model = YOLO(str(weights_path))
        self._pose_model.to("cuda")
        load_ms = (time.perf_counter() - load_t0) * 1000.0
        logger.info(
            f"AvatarCharacter/linux: YOLOv8n-pose loaded onto cuda:0 "
            f"in {load_ms:.1f} ms"
        )

        # Pre-allocate a CPU staging tensor + numpy view for the
        # camera-bytes → cuda hop and the camera-bytes → ModernGL upload
        # path. Same memory backs both.
        self._cam_staging_cpu = torch.zeros(
            (self._H, self._W, 4), dtype=torch.uint8
        ).contiguous()
        self._cam_staging_np = self._cam_staging_cpu.numpy()

        # ModernGL state — initialized lazily inside the first
        # `acquire_write` call, where the adapter's EGL context is current
        # on this thread.
        self._mgl_ctx = None
        self._overlay_renderer = None
        self._mgl_external_color = None
        self._mgl_output_fbo = None

        self._wallclock_t0 = time.monotonic()

        logger.info(
            f"AvatarCharacter/linux: setup complete "
            f"(cuda_id={self._cuda_camera_id} "
            f"uuid={self._opengl_uuid} {self._W}x{self._H})"
        )

    def _ensure_linux_render_state(self, gl_texture_id: int) -> None:
        """Lazy-init ModernGL + PoseOverlayRenderer + cached external FBO.

        Must be called from inside an `OpenGLContext.acquire_write` scope
        (EGL context current on this thread). Idempotent; only runs
        per-construction work on first call.
        """
        if self._mgl_ctx is None:
            self._mgl_ctx = moderngl.create_context(standalone=False)
            logger.info(
                f"AvatarCharacter/linux: ModernGL context created "
                f"(GL {self._mgl_ctx.version_code})"
            )
            self._overlay_renderer = PoseOverlayRenderer(
                self._mgl_ctx, self._W, self._H
            )

        if self._mgl_output_fbo is None:
            # Wrap the imported GL_TEXTURE_2D as a ModernGL external_texture
            # and bind it as the FBO color attachment. The wrapper does NOT
            # own the underlying GL name — releasing it only frees ModernGL
            # bookkeeping. The opengl adapter's gl_texture_id is stable
            # across `acquire_write` calls, so building the FBO once is
            # safe.
            # `external_texture(glo, size, components, samples, dtype)` —
            # ModernGL requires all five positional args (no defaults in
            # the moderngl 5.x signature). The opengl adapter allocates
            # the imported texture as 8-bit RGBA, single-sample.
            self._mgl_external_color = self._mgl_ctx.external_texture(
                gl_texture_id, (self._W, self._H), 4, 0, "f1",
            )
            self._mgl_output_fbo = self._mgl_ctx.framebuffer(
                color_attachments=[self._mgl_external_color],
            )
            logger.info(
                f"AvatarCharacter/linux: external FBO bound (gl_texture_id="
                f"{gl_texture_id} {self._W}x{self._H})"
            )

    def _read_camera_bytes_into_staging(self, ctx, surface_id) -> bool:
        """Resolve the camera DMA-BUF surface and copy its bytes into the
        pre-allocated CPU staging tensor at surface dimensions. Returns
        True on success, False on resolve failure.
        """
        try:
            handle = ctx.gpu_limited_access.resolve_surface(surface_id)
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"AvatarCharacter/linux: resolve_surface({surface_id!r}) "
                    f"failed: {e}"
                )
            return False

        try:
            handle.lock(read_only=True)
            try:
                cam_w = int(handle.width)
                cam_h = int(handle.height)
                bpr = int(handle.bytes_per_row)
                base = handle.base_address
                if not base:
                    raise RuntimeError("base_address null after lock")
                row_stride_pixels = bpr // 4
                buf_type = ctypes.c_uint8 * (bpr * cam_h)
                cam_view = np.frombuffer(
                    buf_type.from_address(base), dtype=np.uint8,
                ).reshape(cam_h, row_stride_pixels, 4)[:, :cam_w, :]
                if (cam_w, cam_h) != (self._W, self._H):
                    cam_resized = cv2.resize(
                        cam_view, (self._W, self._H),
                        interpolation=cv2.INTER_LINEAR,
                    )
                else:
                    cam_resized = np.ascontiguousarray(cam_view)
                # Stage into the persistent torch tensor — `copy_` is
                # contiguous so the next h2d DMA is a single transfer.
                # The numpy view (`_cam_staging_np`) aliases the same
                # memory and is what ModernGL uploads.
                self._cam_staging_cpu.copy_(
                    torch.from_numpy(cam_resized), non_blocking=False,
                )
            finally:
                handle.unlock(read_only=True)
        finally:
            handle.release()
        return True

    def _process_linux(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        upstream_id = frame.get("surface_id")
        if upstream_id is None:
            return

        # 1. Camera bytes → CPU staging tensor.
        if not self._read_camera_bytes_into_staging(ctx, upstream_id):
            return

        # 2. CPU staging → cuda OPAQUE_FD buffer (h2d via the imported
        #    HOST_VISIBLE memory mapped as cuda:0).
        try:
            with self._cuda.acquire_write(self._cuda_camera_id) as view:
                tensor_flat = torch.from_dlpack(view.dlpack)
                tensor_hwc = tensor_flat.view(self._H, self._W, 4)
                tensor_hwc.copy_(self._cam_staging_cpu, non_blocking=False)
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"AvatarCharacter/linux: cuda acquire_write failed: {e}"
                )
            return

        # 3. cuda buffer → YOLOv8n-pose inference (zero-copy CUDA tensor).
        # YOLO requires HxW divisible by stride 32 — we resize on-GPU to
        # 640×640 (native imgsz) before inference, then rescale the
        # returned keypoints back into surface-space for the overlay.
        try:
            with self._cuda.acquire_read(self._cuda_camera_id) as view:
                tensor_flat = torch.from_dlpack(view.dlpack)
                tensor_hwc_bgra = tensor_flat.view(self._H, self._W, 4)
                tensor_rgb = tensor_hwc_bgra[:, :, [2, 1, 0]]
                tensor_chw = tensor_rgb.permute(2, 0, 1).contiguous()
                tensor_bchw = tensor_chw.unsqueeze(0).float() / 255.0
                tensor_bchw_640 = torch.nn.functional.interpolate(
                    tensor_bchw, size=(640, 640),
                    mode="bilinear", align_corners=False,
                )
                results = self._pose_model.predict(
                    tensor_bchw_640, verbose=False, save=False,
                )
                kp_xy = None
                kp_conf = None
                if results and len(results) > 0 and results[0].keypoints is not None:
                    kp = results[0].keypoints
                    if kp.xy is not None and kp.conf is not None:
                        kp_xy = kp.xy.detach().cpu().numpy()      # (N, 17, 2)
                        kp_conf = kp.conf.detach().cpu().numpy()  # (N, 17)
                        # Rescale 640-space keypoints into surface space
                        # for the overlay renderer.
                        if kp_xy.size > 0:
                            kp_xy[..., 0] *= self._W / 640.0
                            kp_xy[..., 1] *= self._H / 640.0
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"AvatarCharacter/linux: cuda acquire_read / inference failed: {e}"
                )
            kp_xy, kp_conf = None, None

        # Pick person 0 (highest-confidence detection if YOLO returned multiple).
        if kp_xy is not None and kp_conf is not None and len(kp_xy) > 0:
            person_xyc = np.concatenate(
                [kp_xy[0], kp_conf[0][..., None]], axis=-1
            )  # (17, 3)
            self._last_keypoints = person_xyc

        # 4. ModernGL render: cyberpunk-tinted camera bg + neon skeleton.
        try:
            with self._opengl.acquire_write(self._opengl_uuid) as view:
                self._ensure_linux_render_state(view.gl_texture_id)
                # Upload camera bytes as the background sampler texture.
                self._overlay_renderer.update_camera_texture(
                    self._cam_staging_np
                )
                t = time.monotonic() - self._wallclock_t0
                self._overlay_renderer.render(
                    self._mgl_output_fbo, self._last_keypoints, t,
                )
        except Exception as e:
            if self.frame_count % 60 == 0:
                logger.warning(
                    f"AvatarCharacter/linux: opengl acquire_write / render failed: {e}"
                )
            return

        # 5. Publish output frame referencing the pre-registered output
        # surface UUID. Display resolves the same UUID via surface-share +
        # the local GpuContext texture cache. No ready-delay on Linux —
        # the macOS path's 1.5s gate was for a Breaking-News-PiP slide-in
        # animation that doesn't apply here; output flows from frame 0.
        out_frame = dict(frame)
        out_frame["surface_id"] = self._opengl_uuid
        out_frame["width"] = self._W
        out_frame["height"] = self._H
        ctx.outputs.write("video_out", out_frame)

        self.frame_count += 1
        if self.frame_count == 1:
            logger.info(
                f"AvatarCharacter/linux: First frame processed "
                f"({self._W}x{self._H})"
            )
        if self.frame_count % 300 == 0:
            logger.debug(
                f"AvatarCharacter/linux: {self.frame_count} frames"
            )

    def _teardown_linux(self, ctx: RuntimeContextFullAccess) -> None:
        if self._overlay_renderer is not None:
            try:
                self._overlay_renderer.release()
            except Exception:
                pass
        if self._mgl_output_fbo is not None:
            try:
                self._mgl_output_fbo.release()
            except Exception:
                pass
        # The external_texture wrapper does NOT own the GL name; releasing
        # it only frees ModernGL's bookkeeping and is safe to skip.
        logger.info(
            f"AvatarCharacter/linux: Shutdown ({self.frame_count} frames)"
        )
