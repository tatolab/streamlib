"""Export YOLOv8m model to CoreML format."""

from ultralytics import YOLO

print("Downloading YOLOv8m model...")
model = YOLO("yolov8m.pt")

print("Exporting to CoreML with NMS...")
model.export(format="coreml", nms=True, imgsz=640)

print("Export complete! yolov8m.mlpackage created")
