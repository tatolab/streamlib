# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot CUDA inference processor — Python.

End-to-end gate for the CUDA subprocess runtime (#591). The host
pre-allocates one HOST_VISIBLE OPAQUE_FD-exportable ``VkBuffer`` and
one exportable timeline semaphore, registers the pair via
surface-share so the subprocess can ``check_out`` both FDs in one
shot. This processor receives a trigger Videoframe, opens the host
surface through ``CudaContext.acquire_write`` to upload a real test
image, then through ``CudaContext.acquire_read`` to run YOLOv8n via
``torch.from_dlpack`` zero-copy on the imported CUDA memory, and
writes an annotated PNG with the model's bounding-box predictions.

Reads the PNG with the Read tool to confirm the model produced
detections — the visual gate that decides whether the polyglot
subprocess actually round-tripped real data through the OPAQUE_FD
+ DLPack + torch + YOLO chain.

Config keys:
    cuda_surface_id (int, required)
        Host-assigned u64 surface id the host pre-registered with
        the cuda adapter.
    width, height (int, required)
        Surface dimensions. The cuda FFI does not thread these through
        the DLPack capsule (the buffer is flat bytes from CUDA's
        perspective); the processor needs them to reshape the
        imported tensor.
    channels (int, required)
        Bytes per pixel of the host buffer. Always ``4`` for the
        BGRA8 surface this scenario allocates.
    output_path (str, required)
        Path the annotated PNG is written to.
"""

from __future__ import annotations

import os
import sys
import time
import urllib.request
from pathlib import Path
from typing import Any, Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.cuda import CudaContext


# Ultralytics' demo asset — same image their docs use to demonstrate
# YOLOv8 detections. Cached locally on first run; not committed to
# the streamlib repo so we sidestep license entanglement.
_TEST_IMAGE_URL = "https://ultralytics.com/images/bus.jpg"


class CudaInferenceProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._surface_id = int(cfg["cuda_surface_id"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._channels = int(cfg["channels"])
        self._output_path = Path(cfg["output_path"])
        self._cuda = CudaContext.from_runtime(ctx)
        self._inferred = False
        self._error: Optional[str] = None

        if self._channels != 4:
            raise ValueError(
                f"[CudaInference/py] expected channels=4 (BGRA8), got "
                f"channels={self._channels} — the v1 scenario only ships "
                "the BGRA8 OPAQUE_FD surface shape"
            )

        # Defer torch + ultralytics imports until setup so that any
        # missing-dependency error surfaces with a clear message instead
        # of breaking module import.
        import torch  # noqa: F401
        from ultralytics import YOLO

        if not torch.cuda.is_available():
            raise RuntimeError(
                "[CudaInference/py] torch.cuda.is_available() == False — "
                "no CUDA-capable PyTorch wheel is installed, or the "
                "process can't see the GPU. The cuda adapter's DLPack "
                "capsule is on a CUDA device; without torch.cuda no "
                "consumer can claim it."
            )
        self._torch = torch

        device_name = torch.cuda.get_device_name(0)
        torch_version = torch.__version__
        print(
            f"[CudaInference/py] torch {torch_version} on CUDA "
            f"device 0 ({device_name})",
            flush=True,
        )

        # Load YOLOv8n. Ultralytics caches the .pt weights under
        # ~/.cache (default) so the first run downloads ~6 MB and
        # subsequent runs are instant.
        load_t0 = time.perf_counter()
        self._model = YOLO("yolov8n.pt")
        # `model.to('cuda')` lazy-initializes the underlying torch
        # module. Force a warmup forward pass so the first real
        # inference doesn't pay the JIT / TF32 init cost — keeps the
        # latency baseline measurement honest.
        self._model.to("cuda")
        load_ms = (time.perf_counter() - load_t0) * 1000.0
        print(
            f"[CudaInference/py] YOLOv8n loaded onto cuda:0 in "
            f"{load_ms:.1f} ms",
            flush=True,
        )

        # Cache the test image as a CPU BGRA8 tensor at the surface's
        # exact dimensions so `acquire_write` is a single
        # `tensor.copy_` — no per-frame decode.
        self._test_image_bgra = self._load_test_image_bgra()
        print(
            f"[CudaInference/py] setup complete — surface_id="
            f"{self._surface_id} {self._width}x{self._height} BGRA8, "
            f"output={self._output_path}",
            flush=True,
        )

    def _load_test_image_bgra(self):
        """Download / cache the test image, decode + resize to the
        surface dimensions, return as a contiguous CPU uint8 tensor of
        shape ``(H, W, 4)`` in BGRA8 layout (alpha = 255).

        BGRA8 because the host allocates the OPAQUE_FD VkBuffer as
        BGRA32 (4 bytes/pixel), matching the rest of streamlib's
        pixel-format conventions.
        """
        torch = self._torch

        cache_dir = Path.home() / ".cache" / "streamlib-cuda-inference"
        cache_dir.mkdir(parents=True, exist_ok=True)
        img_path = cache_dir / "bus.jpg"
        if not img_path.exists():
            print(
                f"[CudaInference/py] downloading test image: "
                f"{_TEST_IMAGE_URL} -> {img_path}",
                flush=True,
            )
            urllib.request.urlretrieve(_TEST_IMAGE_URL, img_path)

        # cv2 reads as BGR uint8 (H, W, 3). Resize to the surface
        # dimensions and pad alpha = 255 for BGRA.
        import cv2

        bgr = cv2.imread(str(img_path), cv2.IMREAD_COLOR)
        if bgr is None:
            raise RuntimeError(
                f"[CudaInference/py] cv2.imread failed for {img_path} — "
                "the cached test image may be corrupted; delete the "
                "cache directory and re-run."
            )
        bgr_resized = cv2.resize(
            bgr, (self._width, self._height), interpolation=cv2.INTER_AREA
        )
        bgra = cv2.cvtColor(bgr_resized, cv2.COLOR_BGR2BGRA)
        # cv2 returns numpy; convert to a contiguous torch tensor on
        # CPU so the GPU upload is a single contiguous DMA.
        t = torch.from_numpy(bgra).contiguous()  # (H, W, 4) uint8
        return t

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Drain the trigger frame so the upstream port doesn't
        # backpressure.
        _frame = ctx.inputs.read("video_in")
        if _frame is None:
            return
        # One-shot — repeat invocations would re-run inference on the
        # same buffer, which still validates the wire path but obscures
        # latency baselines.
        if self._inferred:
            return
        try:
            self._run_once()
            self._inferred = True
        except Exception as e:
            self._error = str(e)
            print(
                f"[CudaInference/py] inference failed: {e}",
                flush=True,
                file=sys.stderr,
            )
            # Keep the runtime alive so the host's stop signal still
            # flushes the timeline — we surface the error in teardown.

    def _run_once(self) -> None:
        torch = self._torch
        h, w = self._height, self._width

        # ── Upload phase: write the test image into the host's
        #    OPAQUE_FD buffer via CudaContext.acquire_write. The
        #    DLPack capsule's underlying CUDA memory IS the host's
        #    OPAQUE_FD buffer (zero-copy mapping); writing to a
        #    torch tensor view of the capsule alias-writes the host
        #    surface.
        upload_t0 = time.perf_counter()
        with self._cuda.acquire_write(self._surface_id) as view:
            self._validate_view(view, write=True)
            tensor_flat = torch.from_dlpack(view.dlpack)  # (N,) uint8
            # Per the python adapter (`cuda.py` _build_view): width /
            # height are not threaded through FFI, so the DLPack
            # tensor lands flat. Reshape using the surface dimensions
            # the host advertised in this processor's config.
            tensor_hwc = tensor_flat.view(h, w, 4)
            # Stage the upload: copy from the cached CPU BGRA tensor
            # to the imported CUDA tensor in place. `copy_` runs the
            # h2d DMA on the current torch CUDA stream.
            tensor_hwc.copy_(self._test_image_bgra, non_blocking=False)
        upload_ms = (time.perf_counter() - upload_t0) * 1000.0
        print(
            f"[CudaInference/py] uploaded {h}x{w}x4 BGRA8 to OPAQUE_FD "
            f"surface in {upload_ms:.2f} ms (acquire_write + tensor.copy_)",
            flush=True,
        )

        # ── Inference phase: re-acquire the surface as a read view
        #    and run YOLO directly on the CUDA-resident tensor. The
        #    write→read sequence advances the timeline twice (1 → 2),
        #    proving the sync surface works under round-trip use.
        infer_t0 = time.perf_counter()
        with self._cuda.acquire_read(self._surface_id) as view:
            self._validate_view(view, write=False)
            tensor_flat = torch.from_dlpack(view.dlpack)
            tensor_hwc_bgra = tensor_flat.view(h, w, 4)

            # YOLO ingests RGB uint8/float CHW. Our buffer is BGRA8,
            # so: drop the alpha (channels 0..3 → 0..2 == BGR), swap
            # to RGB ([2,1,0]), permute HWC→CHW, add batch dim, scale
            # uint8 → float [0, 1]. All on CUDA — no host roundtrip.
            tensor_rgb = tensor_hwc_bgra[:, :, [2, 1, 0]]  # BGRA → RGB
            tensor_chw = tensor_rgb.permute(2, 0, 1).contiguous()  # (3, H, W)
            tensor_bchw = tensor_chw.unsqueeze(0).float() / 255.0  # (1, 3, H, W)

            acquire_ms = (time.perf_counter() - infer_t0) * 1000.0

            # Run inference. ultralytics' model.predict accepts
            # tensor input already in (B, 3, H, W) float; it will run
            # on the same CUDA device.
            model_t0 = time.perf_counter()
            results = self._model.predict(
                source=tensor_bchw, verbose=False, save=False
            )
            model_ms = (time.perf_counter() - model_t0) * 1000.0

            # ``results[0].plot()`` renders the input image with
            # bounding boxes, class labels, and confidences as a
            # numpy BGR array. Save inside the read scope so the
            # underlying tensor is still valid (the plot reads from
            # the input tensor's CPU mirror; we slice once for safety).
            annotated_bgr = results[0].plot()

        infer_total_ms = (time.perf_counter() - infer_t0) * 1000.0
        n_detections = (
            len(results[0].boxes) if results and results[0].boxes is not None else 0
        )
        print(
            f"[CudaInference/py] inference: acquire+pre={acquire_ms:.2f} ms, "
            f"model={model_ms:.2f} ms, total={infer_total_ms:.2f} ms, "
            f"detections={n_detections}",
            flush=True,
        )

        # Write the annotated PNG.
        import cv2

        self._output_path.parent.mkdir(parents=True, exist_ok=True)
        ok = cv2.imwrite(str(self._output_path), annotated_bgr)
        if not ok:
            raise RuntimeError(
                f"[CudaInference/py] cv2.imwrite returned False for "
                f"{self._output_path}"
            )
        print(
            f"[CudaInference/py] wrote annotated PNG: {self._output_path} "
            f"({n_detections} detections)",
            flush=True,
        )

    def _validate_view(self, view: Any, write: bool) -> None:
        """Validate the cdylib-emitted DLPack view shape.

        Accepts only ``device_type == kDLCUDA (2)`` — ``torch.from_dlpack``
        in the versions we target does not consume capsules typed as
        ``kDLCUDAHost (3)`` (host-pinned, CUDA-accessible memory) and
        would fail downstream with a confusing torch error. The cdylib
        hard-codes ``kDLCUDA`` today; #588 Stage 8 wired a
        ``cudaPointerGetAttributes`` probe but did not flip the default.
        If this validator ever fires on type 3, the cdylib's default
        flipped — surface that here rather than letting torch raise
        a generic error.
        """
        kind = "write" if write else "read"
        expected_size = self._width * self._height * self._channels
        if view.size != expected_size:
            raise RuntimeError(
                f"[CudaInference/py] {kind} view size mismatch — "
                f"expected {expected_size} bytes (w*h*c), got "
                f"{view.size}. Host buffer dimensions in the "
                "scenario binary disagree with this processor's "
                "config."
            )
        if view.device_type != 2:  # kDLCUDA
            if view.device_type == 3:  # kDLCUDAHost
                detail = (
                    "kDLCUDAHost (3) means the cdylib's "
                    "cudaPointerGetAttributes probe (#588 Stage 8) "
                    "returned host-pinned memory; torch.from_dlpack "
                    "does not consume kDLCUDAHost capsules"
                )
            else:
                detail = "not a CUDA device type"
            raise RuntimeError(
                f"[CudaInference/py] {kind} view device_type="
                f"{view.device_type} — expected kDLCUDA (2): {detail}"
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        status = "ok" if self._inferred and self._error is None else "fail"
        print(
            f"[CudaInference/py] teardown status={status} "
            f"inferred={self._inferred} error={self._error}",
            flush=True,
        )
