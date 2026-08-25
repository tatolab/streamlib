# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The pose processor's failure handling, with the detector stood in for.

A host whose system cuDNN disagrees with torch's bundled one fails every
convolution; the handler's contract is: heal that one failure by disabling
cuDNN and retrying, degrade everything else to an empty skeleton, and never
let a detection failure take the layer's cadence down.
"""

from __future__ import annotations

import pytest
import torch

from camera_python_effects.pose_keypoint_packing import COCO_KEYPOINT_COUNT
from camera_python_effects.processors.pose_skeleton_overlay import PoseSkeletonOverlay

A_CUDNN_FAILURE = RuntimeError(
    "CUDNN_BACKEND_TENSOR_DESCRIPTOR cudnnFinalize failed"
    "ptrDesc->finalize() cudnn_status: CUDNN_STATUS_SUBLIBRARY_VERSION_MISMATCH"
)

A_DETECTED_POSE = ([7] * COCO_KEYPOINT_COUNT, 0b101)

EMPTY_SKELETON = ([0] * COCO_KEYPOINT_COUNT, 0)


@pytest.fixture
def processor_that_never_loaded_a_model() -> PoseSkeletonOverlay:
    """The handler under test needs no engine, no GPU, and no model."""
    processor = PoseSkeletonOverlay()
    processor.a_detection_failure_has_been_reported = False
    return processor


@pytest.fixture
def cudnn_starts_enabled(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(torch.backends.cudnn, "enabled", True)


def test_a_cudnn_failure_disables_cudnn_and_retries(
    processor_that_never_loaded_a_model: PoseSkeletonOverlay,
    cudnn_starts_enabled: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    retries: "list[bool]" = []

    def a_retry_that_succeeds(frame: object) -> "tuple[list[int], int]":
        retries.append(torch.backends.cudnn.enabled)
        return A_DETECTED_POSE

    monkeypatch.setattr(
        processor_that_never_loaded_a_model, "detect_pose_in", a_retry_that_succeeds
    )
    detected = processor_that_never_loaded_a_model.detect_after_a_failure(
        object(), A_CUDNN_FAILURE
    )
    assert detected == A_DETECTED_POSE
    # The retry ran, and ran with cuDNN already off.
    assert retries == [False]


def test_a_failure_that_is_not_cudnn_shaped_degrades_without_a_retry(
    processor_that_never_loaded_a_model: PoseSkeletonOverlay,
    cudnn_starts_enabled: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def a_detector_that_must_not_run(frame: object) -> "tuple[list[int], int]":
        raise AssertionError("an out-of-memory must not turn cuDNN off")

    monkeypatch.setattr(
        processor_that_never_loaded_a_model, "detect_pose_in", a_detector_that_must_not_run
    )
    detected = processor_that_never_loaded_a_model.detect_after_a_failure(
        object(), RuntimeError("CUDA out of memory")
    )
    assert detected == EMPTY_SKELETON
    assert torch.backends.cudnn.enabled


def test_a_second_cudnn_failure_degrades_instead_of_looping(
    processor_that_never_loaded_a_model: PoseSkeletonOverlay,
    cudnn_starts_enabled: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def a_retry_that_also_fails(frame: object) -> "tuple[list[int], int]":
        raise A_CUDNN_FAILURE

    monkeypatch.setattr(
        processor_that_never_loaded_a_model, "detect_pose_in", a_retry_that_also_fails
    )
    detected = processor_that_never_loaded_a_model.detect_after_a_failure(
        object(), A_CUDNN_FAILURE
    )
    assert detected == EMPTY_SKELETON
    # And with cuDNN already off, a later cuDNN-worded failure retries no more.
    detected_again = processor_that_never_loaded_a_model.detect_after_a_failure(
        object(), A_CUDNN_FAILURE
    )
    assert detected_again == EMPTY_SKELETON
