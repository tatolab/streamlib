"""
Model-specific postprocessing functions.

Converts raw ML model outputs to standardized detection format:
{
    'boxes': [(x, y, w, h), ...],
    'classes': [class_id, ...],
    'scores': [confidence, ...],
    'names': ['person', 'car', ...]
}
"""

import numpy as np
from typing import Dict, Any, List, Tuple


# COCO class names (what YOLOv8 is trained on)
COCO_CLASSES = [
    'person', 'bicycle', 'car', 'motorcycle', 'airplane', 'bus', 'train', 'truck', 'boat',
    'traffic light', 'fire hydrant', 'stop sign', 'parking meter', 'bench', 'bird', 'cat',
    'dog', 'horse', 'sheep', 'cow', 'elephant', 'bear', 'zebra', 'giraffe', 'backpack',
    'umbrella', 'handbag', 'tie', 'suitcase', 'frisbee', 'skis', 'snowboard', 'sports ball',
    'kite', 'baseball bat', 'baseball glove', 'skateboard', 'surfboard', 'tennis racket',
    'bottle', 'wine glass', 'cup', 'fork', 'knife', 'spoon', 'bowl', 'banana', 'apple',
    'sandwich', 'orange', 'broccoli', 'carrot', 'hot dog', 'pizza', 'donut', 'cake', 'chair',
    'couch', 'potted plant', 'bed', 'dining table', 'toilet', 'tv', 'laptop', 'mouse', 'remote',
    'keyboard', 'cell phone', 'microwave', 'oven', 'toaster', 'sink', 'refrigerator', 'book',
    'clock', 'vase', 'scissors', 'teddy bear', 'hair drier', 'toothbrush'
]


def yolov8_postprocess(
    outputs: Dict[str, np.ndarray],
    conf_threshold: float = 0.25,
    iou_threshold: float = 0.45,
    max_detections: int = 300
) -> Dict[str, Any]:
    """
    Postprocess YOLOv8 ONNX output to extract detections.

    YOLOv8 output format: (1, 84, 8400)
    - 8400 predictions (grid cells)
    - 84 values per prediction: [x, y, w, h, class0_conf, class1_conf, ..., class79_conf]

    Args:
        outputs: Raw ONNX outputs {'output0': (1, 84, 8400)}
        conf_threshold: Minimum confidence threshold
        iou_threshold: IoU threshold for NMS
        max_detections: Maximum number of detections to return

    Returns:
        {
            'boxes': [(x, y, w, h), ...],
            'classes': [class_id, ...],
            'scores': [confidence, ...],
            'names': ['person', ...]
        }
    """
    # Get raw output (1, 84, 8400)
    output = outputs.get('output0', None)
    if output is None:
        raise ValueError("YOLOv8 output 'output0' not found")

    # Transpose to (1, 8400, 84)
    output = np.transpose(output, (0, 2, 1))

    # Extract boxes and class probabilities
    predictions = output[0]  # (8400, 84)
    boxes_xywh = predictions[:, :4]  # (8400, 4) - center x, y, width, height
    class_probs = predictions[:, 4:]  # (8400, 80) - class probabilities

    # Get class with highest probability for each prediction
    class_ids = np.argmax(class_probs, axis=1)  # (8400,)
    confidences = np.max(class_probs, axis=1)  # (8400,)

    # Filter by confidence threshold
    mask = confidences > conf_threshold
    boxes_xywh = boxes_xywh[mask]
    class_ids = class_ids[mask]
    confidences = confidences[mask]

    if len(boxes_xywh) == 0:
        return {
            'boxes': [],
            'classes': [],
            'scores': [],
            'names': []
        }

    # Convert boxes from xywh (center) to xyxy (corners) for NMS
    boxes_xyxy = xywh_to_xyxy(boxes_xywh)

    # Apply Non-Maximum Suppression (NMS)
    keep_indices = nms(boxes_xyxy, confidences, iou_threshold)

    # Limit to max detections
    keep_indices = keep_indices[:max_detections]

    # Extract final detections
    final_boxes = boxes_xywh[keep_indices]
    final_classes = class_ids[keep_indices]
    final_scores = confidences[keep_indices]

    # Convert to list of tuples for easy use
    boxes_list = [(float(x), float(y), float(w), float(h)) for x, y, w, h in final_boxes]
    classes_list = [int(c) for c in final_classes]
    scores_list = [float(s) for s in final_scores]
    names_list = [COCO_CLASSES[c] if c < len(COCO_CLASSES) else f'class{c}' for c in classes_list]

    return {
        'boxes': boxes_list,
        'classes': classes_list,
        'scores': scores_list,
        'names': names_list
    }


def xywh_to_xyxy(boxes_xywh: np.ndarray) -> np.ndarray:
    """
    Convert boxes from xywh (center x, center y, width, height)
    to xyxy (x1, y1, x2, y2) format.

    Args:
        boxes_xywh: (N, 4) array in xywh format

    Returns:
        (N, 4) array in xyxy format
    """
    boxes_xyxy = np.copy(boxes_xywh)
    boxes_xyxy[:, 0] = boxes_xywh[:, 0] - boxes_xywh[:, 2] / 2  # x1 = cx - w/2
    boxes_xyxy[:, 1] = boxes_xywh[:, 1] - boxes_xywh[:, 3] / 2  # y1 = cy - h/2
    boxes_xyxy[:, 2] = boxes_xywh[:, 0] + boxes_xywh[:, 2] / 2  # x2 = cx + w/2
    boxes_xyxy[:, 3] = boxes_xywh[:, 1] + boxes_xywh[:, 3] / 2  # y2 = cy + h/2
    return boxes_xyxy


def nms(boxes: np.ndarray, scores: np.ndarray, iou_threshold: float) -> List[int]:
    """
    Non-Maximum Suppression (NMS) - removes overlapping boxes.

    Args:
        boxes: (N, 4) boxes in xyxy format
        scores: (N,) confidence scores
        iou_threshold: IoU threshold for suppression

    Returns:
        List of indices to keep
    """
    # Sort by score (descending)
    order = scores.argsort()[::-1]

    keep = []
    while len(order) > 0:
        i = order[0]
        keep.append(i)

        if len(order) == 1:
            break

        # Compute IoU of the kept box with the rest
        ious = compute_iou(boxes[i], boxes[order[1:]])

        # Remove boxes with high IoU (overlapping)
        order = order[1:][ious <= iou_threshold]

    return keep


def compute_iou(box: np.ndarray, boxes: np.ndarray) -> np.ndarray:
    """
    Compute IoU between one box and multiple boxes.

    Args:
        box: (4,) single box in xyxy format
        boxes: (N, 4) multiple boxes in xyxy format

    Returns:
        (N,) IoU values
    """
    # Intersection area
    x1 = np.maximum(box[0], boxes[:, 0])
    y1 = np.maximum(box[1], boxes[:, 1])
    x2 = np.minimum(box[2], boxes[:, 2])
    y2 = np.minimum(box[3], boxes[:, 3])

    intersection = np.maximum(0, x2 - x1) * np.maximum(0, y2 - y1)

    # Union area
    box_area = (box[2] - box[0]) * (box[3] - box[1])
    boxes_area = (boxes[:, 2] - boxes[:, 0]) * (boxes[:, 3] - boxes[:, 1])
    union = box_area + boxes_area - intersection

    # IoU
    iou = intersection / (union + 1e-6)

    return iou


def yolov8_coreml_postprocess(
    outputs: Dict[str, Any],
    conf_threshold: float = 0.25,
    input_size: int = 640
) -> Dict[str, Any]:
    """
    Postprocess YOLOv8 CoreML output (NMS already applied).

    CoreML YOLOv8 output format:
    - coordinates: (N, 4) - normalized [0,1] center_x, center_y, width, height
    - confidence: (N, 80) - class scores for each detection

    Args:
        outputs: CoreML outputs {'coordinates': (N, 4), 'confidence': (N, 80)}
        conf_threshold: Minimum confidence threshold
        input_size: Model input size (640 for YOLOv8n)

    Returns:
        {
            'boxes': [(x, y, w, h), ...],  # In model input coordinates (0-640)
            'classes': [class_id, ...],
            'scores': [confidence, ...],
            'names': ['person', ...]
        }
    """
    # Get outputs (already converted to numpy by backend)
    coordinates = outputs.get('coordinates', None)
    confidence = outputs.get('confidence', None)

    if coordinates is None or confidence is None:
        raise ValueError("CoreML outputs 'coordinates' and 'confidence' not found")

    # Ensure they are numpy arrays
    if not isinstance(coordinates, np.ndarray):
        coordinates = np.asarray(coordinates)
    if not isinstance(confidence, np.ndarray):
        confidence = np.asarray(confidence)

    # Get number of detections
    num_detections = coordinates.shape[0]

    if num_detections == 0:
        return {
            'boxes': [],
            'classes': [],
            'scores': [],
            'names': []
        }

    # Find best class for each detection
    class_ids = np.argmax(confidence, axis=1)  # (N,)
    scores = np.max(confidence, axis=1)  # (N,)

    # Filter by confidence threshold
    mask = scores > conf_threshold
    filtered_coords = coordinates[mask]
    filtered_classes = class_ids[mask]
    filtered_scores = scores[mask]

    if len(filtered_coords) == 0:
        return {
            'boxes': [],
            'classes': [],
            'scores': [],
            'names': []
        }

    # Scale normalized coordinates [0,1] to model input size [0,640]
    scaled_coords = filtered_coords * input_size

    # Convert to list format
    boxes_list = [(float(x), float(y), float(w), float(h)) for x, y, w, h in scaled_coords]
    classes_list = [int(c) for c in filtered_classes]
    scores_list = [float(s) for s in filtered_scores]
    names_list = [COCO_CLASSES[c] if c < len(COCO_CLASSES) else f'class{c}' for c in classes_list]

    return {
        'boxes': boxes_list,
        'classes': classes_list,
        'scores': scores_list,
        'names': names_list
    }


__all__ = ['yolov8_postprocess', 'yolov8_coreml_postprocess', 'COCO_CLASSES']
