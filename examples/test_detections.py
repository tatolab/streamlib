"""
Object detection example with ML inference and visual compositing.

This example demonstrates:
1. Camera source streaming at 30 FPS
2. ML inference running independently (may be slower than 30 FPS)
3. Detection results composited onto video in real-time
4. Video pipeline never blocks on ML processing

Architecture:
    Camera ──┬──> ML Detector ─(data)──> Compositor ──> Display
             └─────────(video)───────────────┘

The compositor always passes video through, drawing detection boxes
when they're available from the ML pipeline.
"""

import asyncio
from streamlib import (
    StreamRuntime, Stream,
    camera_source, display_sink,
    stream_processor,
    VideoInput, VideoOutput, DataInput, DataOutput,
    VideoFrame, DataMessage
)
from streamlib.gpu import GPUContext


# ============================================================================
# Ideal ML Detector Decorator (to be implemented)
# ============================================================================
# This is what we WANT the API to look like:
#
# from streamlib import ml_detector
#
# @ml_detector(
#     model_path="yolov8n.onnx",
#     input_size=(640, 640),
#     confidence_threshold=0.5,
#     preprocess="yolo",  # Built-in preprocessing
#     postprocess="yolo"  # Built-in NMS + filtering
# )
# def object_detector():
#     """
#     Zero-code ML inference!
#     - Automatically downloads texture from GPU
#     - Preprocesses (resize, normalize) for YOLO
#     - Runs ONNX inference with native acceleration (CoreML/DirectML/CUDA)
#     - Postprocesses (NMS, filtering)
#     - Outputs DataMessage with detections
#     """
#     pass
#
# This would create a handler with:
#   inputs:  VideoInput('video')
#   outputs: DataOutput('detections')
#
# ============================================================================


# For now, we'll implement it manually using @stream_processor
@stream_processor(
    inputs={'video': VideoInput('video')},
    outputs={'detections': DataOutput('detections')}
)
async def object_detector(tick, inputs, outputs):
    """
    Object detection using YOLO via ONNX Runtime.

    Runs ML inference independently from video pipeline.
    May process at 10-20 FPS even if camera is 30 FPS.
    """
    # Read latest video frame (non-blocking)
    frame = inputs['video'].read_latest()
    if frame is None:
        return

    # Get GPU context for ML inference
    gpu = object_detector._runtime.gpu_context

    # Load model on first frame (lazy initialization)
    if not hasattr(object_detector, 'model'):
        print("[ML] Loading YOLO model...")
        object_detector.model = gpu.ml.load_model("yolov8m.mlpackage")
        print("[ML] Model loaded with native CoreML acceleration")

    try:
        # Run ML inference (20-50ms depending on model/hardware)
        # NOTE: ml.run() needs to be implemented with:
        # 1. Download texture → CPU
        # 2. Preprocess (resize to 640x640, normalize)
        # 3. Run ONNX inference (CoreML/DirectML/CUDA)
        # 4. Postprocess (NMS, filter by confidence)
        # 5. Return detection results
        raw_output = gpu.ml.run(
            object_detector.model,
            frame.data,
            preprocess=True  # Auto resize/normalize for YOLO
        )

        # Postprocess YOLO output to get actual detections
        # CoreML returns NMS-processed output, use CoreML-specific postprocessor
        from streamlib.gpu.ml.postprocess import yolov8_coreml_postprocess
        detections = yolov8_coreml_postprocess(
            raw_output,
            conf_threshold=0.5,  # Standard YOLO threshold - temporal smoothing handles jitter
            input_size=640
        )

        # Filter out known false positives (hands misclassified as toothbrush)
        # COCO doesn't have "hand" class, so YOLO picks closest object (toothbrush)
        filtered_boxes = []
        filtered_classes = []
        filtered_scores = []
        filtered_names = []

        for box, cls, score, name in zip(
            detections['boxes'],
            detections['classes'],
            detections['scores'],
            detections['names']
        ):
            # Skip toothbrush detections (usually hands)
            if name != 'toothbrush':
                filtered_boxes.append(box)
                filtered_classes.append(cls)
                filtered_scores.append(score)
                filtered_names.append(name)

        # Output detection data (not video!)
        detection_msg = DataMessage(
            data={
                'boxes': filtered_boxes,           # [(x, y, w, h), ...]
                'classes': filtered_classes,       # [0, 15, 2, ...] (COCO class IDs)
                'scores': filtered_scores,         # [0.95, 0.87, ...]
                'names': filtered_names,           # ['person', 'car', ...]
                'frame_number': frame.frame_number,
                'timestamp': frame.timestamp
            },
            timestamp=tick.timestamp
        )

        outputs['detections'].write(detection_msg)

        # Log inference rate (not FPS, just successful inferences)
        if not hasattr(object_detector, 'frame_count'):
            object_detector.frame_count = 0
            object_detector.start_time = tick.timestamp

        object_detector.frame_count += 1
        elapsed = tick.timestamp - object_detector.start_time
        if elapsed > 0:
            inference_rate = object_detector.frame_count / elapsed
            if object_detector.frame_count % 30 == 0:
                print(f"[ML] Inference rate: {inference_rate:.1f} FPS")

    except NotImplementedError:
        # ml.run() not implemented yet, output dummy detections
        if not hasattr(object_detector, 'warned'):
            print("[ML] WARNING: ml.run() not implemented, using dummy detections")
            object_detector.warned = True

        # Create dummy detection for testing
        dummy_detection = DataMessage(
            data={
                'boxes': [(100, 100, 200, 150), (400, 300, 150, 200)],
                'classes': [0, 2],  # person, car
                'scores': [0.95, 0.87],
                'frame_number': frame.frame_number,
                'timestamp': frame.timestamp
            },
            timestamp=tick.timestamp
        )
        outputs['detections'].write(dummy_detection)

    except Exception as e:
        print(f"[ML] Error during inference: {e}")


@stream_processor(
    inputs={
        'video': VideoInput('video'),
        'detections': DataInput('detections')
    },
    outputs={'video': VideoOutput('video')}
)
async def draw_detections(tick, inputs, outputs):
    """
    Composite detection boxes onto video frames.

    Always passes video through immediately, drawing boxes when
    detection data is available. Uses cached detections if ML
    is slower than video FPS.
    """
    # Read latest video frame (always available)
    frame = inputs['video'].read_latest()
    if frame is None:
        return

    # Read latest detection data (may be None if ML hasn't output yet)
    detection_msg = inputs['detections'].read_latest()

    # Update cache if we have new detections with temporal smoothing
    if detection_msg:
        if not hasattr(draw_detections, 'cached_detections'):
            print("[Compositor] First detections received!")
            draw_detections.cached_detections = detection_msg.data
            # Initialize tracking state
            draw_detections.smoothed_boxes = detection_msg.data['boxes'].copy()
        else:
            # Temporal smoothing: blend new detections with previous positions
            new_detections = detection_msg.data

            # Simple approach: exponential moving average for box positions
            # This smooths out jitter while still following real movement
            alpha = 0.3  # Smoothing factor (0=no update, 1=full update)

            # Match new detections to previous ones (simple positional matching)
            smoothed_boxes = []
            for i, (new_box, class_id) in enumerate(zip(new_detections['boxes'], new_detections['classes'])):
                # Find if this object was detected before (same class, nearby position)
                best_match_idx = None
                best_distance = float('inf')

                if hasattr(draw_detections, 'cached_detections'):
                    old_boxes = draw_detections.cached_detections['boxes']
                    old_classes = draw_detections.cached_detections['classes']

                    for j, (old_box, old_class) in enumerate(zip(old_boxes, old_classes)):
                        if class_id == old_class:
                            # Calculate distance between centers
                            new_cx, new_cy = new_box[0], new_box[1]
                            old_cx, old_cy = old_box[0], old_box[1]
                            distance = ((new_cx - old_cx)**2 + (new_cy - old_cy)**2)**0.5

                            # Match if within reasonable distance (200 pixels)
                            if distance < 200 and distance < best_distance:
                                best_distance = distance
                                best_match_idx = j

                # Smooth the box position if we found a match
                if best_match_idx is not None:
                    old_box = old_boxes[best_match_idx]
                    # Exponential moving average
                    smoothed_box = (
                        old_box[0] * (1 - alpha) + new_box[0] * alpha,  # cx
                        old_box[1] * (1 - alpha) + new_box[1] * alpha,  # cy
                        old_box[2] * (1 - alpha) + new_box[2] * alpha,  # w
                        old_box[3] * (1 - alpha) + new_box[3] * alpha,  # h
                    )
                    smoothed_boxes.append(smoothed_box)
                else:
                    # New object, no smoothing
                    smoothed_boxes.append(new_box)

            # Update cache with smoothed positions
            draw_detections.cached_detections = {
                'boxes': smoothed_boxes,
                'classes': new_detections['classes'],
                'scores': new_detections['scores'],
                'names': new_detections['names'],
                'frame_number': new_detections['frame_number'],
                'timestamp': new_detections['timestamp']
            }

    # Draw boxes if we have cached detections
    if hasattr(draw_detections, 'cached_detections'):
        detections = draw_detections.cached_detections
        gpu = draw_detections._runtime.gpu_context

        # Draw boxes on GPU using compute shader
        if len(detections['boxes']) > 0:
            import wgpu
            import numpy as np
            from PIL import Image, ImageDraw, ImageFont

            # Create shader pipeline on first use
            if not hasattr(draw_detections, 'box_pipeline'):
                # WGSL shader for drawing boxes
                shader_code = """
                struct Box {
                    x: f32,
                    y: f32,
                    w: f32,
                    h: f32,
                    r: f32,
                    g: f32,
                    b: f32,
                    thickness: f32,
                }

                @group(0) @binding(0) var input_tex: texture_2d<f32>;
                @group(0) @binding(1) var output_tex: texture_storage_2d<rgba8unorm, write>;
                @group(0) @binding(2) var<storage, read> boxes: array<Box>;
                @group(0) @binding(3) var<uniform> num_boxes: u32;

                @compute @workgroup_size(8, 8)
                fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
                    let dims = textureDimensions(input_tex);
                    let pos = vec2<i32>(i32(global_id.x), i32(global_id.y));
                    let dims_i = vec2<i32>(i32(dims.x), i32(dims.y));

                    if (pos.x >= dims_i.x || pos.y >= dims_i.y) {
                        return;
                    }

                    // Read input pixel
                    var color = textureLoad(input_tex, pos, 0);

                    // Check if pixel is on any box edge
                    let fpos = vec2<f32>(f32(pos.x), f32(pos.y));

                    for (var i = 0u; i < num_boxes; i++) {
                        let box = boxes[i];
                        let x1 = box.x - box.w / 2.0;
                        let y1 = box.y - box.h / 2.0;
                        let x2 = box.x + box.w / 2.0;
                        let y2 = box.y + box.h / 2.0;
                        let t = box.thickness;

                        // Check if on edge (within thickness pixels of border)
                        let on_edge = (fpos.x >= x1 && fpos.x <= x2 &&
                                      ((fpos.y >= y1 && fpos.y < y1 + t) ||
                                       (fpos.y >= y2 - t && fpos.y <= y2))) ||
                                     (fpos.y >= y1 && fpos.y <= y2 &&
                                      ((fpos.x >= x1 && fpos.x < x1 + t) ||
                                       (fpos.x >= x2 - t && fpos.x <= x2)));

                        if (on_edge) {
                            color = vec4<f32>(box.r, box.g, box.b, 1.0);
                            break;
                        }
                    }

                    textureStore(output_tex, pos, color);
                }
                """

                shader_module = gpu.device.create_shader_module(code=shader_code)
                draw_detections.box_pipeline = gpu.device.create_compute_pipeline(
                    layout="auto",
                    compute={"module": shader_module, "entry_point": "main"}
                )

            # Prepare box data for GPU
            width = frame.width
            height = frame.height

            # Scale boxes from YOLO coordinates (0-640) to frame size
            scale_x = width / 640.0
            scale_y = height / 640.0

            box_data = []
            for box in detections['boxes'][:32]:  # Limit to 32 boxes for performance
                cx, cy, w, h = box
                # Scale to frame coordinates
                cx *= scale_x
                cy *= scale_y
                w *= scale_x
                h *= scale_y
                # Pack: x, y, w, h, r, g, b, thickness
                box_data.extend([cx, cy, w, h, 0.0, 1.0, 0.0, 3.0])  # Green, 3px thick

            # Pad to at least 1 box
            if not box_data:
                box_data = [0, 0, 0, 0, 0, 0, 0, 0]

            box_array = np.array(box_data, dtype=np.float32)

            # Create GPU buffers
            box_buffer = gpu.device.create_buffer_with_data(
                data=box_array,
                usage=wgpu.BufferUsage.STORAGE
            )

            num_boxes_buffer = gpu.device.create_buffer_with_data(
                data=np.array([len(detections['boxes'][:32])], dtype=np.uint32),
                usage=wgpu.BufferUsage.UNIFORM
            )

            # Create output texture (use rgba8unorm for compute shader support)
            output_texture = gpu.device.create_texture(
                size=(width, height, 1),
                format=wgpu.TextureFormat.rgba8unorm,
                usage=wgpu.TextureUsage.STORAGE_BINDING | wgpu.TextureUsage.COPY_SRC | wgpu.TextureUsage.TEXTURE_BINDING | wgpu.TextureUsage.COPY_DST
            )

            # Create bind group
            bind_group = gpu.device.create_bind_group(
                layout=draw_detections.box_pipeline.get_bind_group_layout(0),
                entries=[
                    {"binding": 0, "resource": frame.data.create_view()},
                    {"binding": 1, "resource": output_texture.create_view()},
                    {"binding": 2, "resource": {"buffer": box_buffer}},
                    {"binding": 3, "resource": {"buffer": num_boxes_buffer}},
                ]
            )

            # Run compute shader
            encoder = gpu.device.create_command_encoder()
            compute_pass = encoder.begin_compute_pass()
            compute_pass.set_pipeline(draw_detections.box_pipeline)
            compute_pass.set_bind_group(0, bind_group)
            compute_pass.dispatch_workgroups((width + 7) // 8, (height + 7) // 8, 1)
            compute_pass.end()
            gpu.queue.submit([encoder.finish()])

            # Add text labels for each detection
            # Create text labels using PIL and composite onto GPU texture
            for box, name, score in zip(
                detections['boxes'][:32],
                detections['names'][:32],
                detections['scores'][:32]
            ):
                # Scale box coordinates to frame size
                cx, cy, w, h = box
                cx *= scale_x
                cy *= scale_y

                # Calculate label position (top-left of box)
                label_x = int(cx - w * scale_x / 2)
                label_y = int(cy - h * scale_y / 2) - 20  # Above the box

                # Create label text
                label_text = f"{name} {score:.2f}"

                # Create text image using PIL (small texture)
                try:
                    # Use default font
                    font = ImageFont.load_default()
                except:
                    font = None

                # Create small image for text
                text_img = Image.new('RGBA', (200, 25), (0, 0, 0, 0))
                draw = ImageDraw.Draw(text_img)

                # Draw background rectangle for readability
                bbox = draw.textbbox((0, 0), label_text, font=font)
                text_width = bbox[2] - bbox[0] + 8
                text_height = bbox[3] - bbox[1] + 4
                draw.rectangle([0, 0, text_width, text_height], fill=(0, 0, 0, 180))

                # Draw text
                draw.text((4, 2), label_text, fill=(0, 255, 0, 255), font=font)

                # Crop to actual text size
                text_img = text_img.crop((0, 0, text_width, text_height))

                # Convert to bytes
                text_bytes = text_img.tobytes()

                # Create GPU texture for text
                text_texture = gpu.device.create_texture(
                    size=(text_width, text_height, 1),
                    format=wgpu.TextureFormat.rgba8unorm,
                    usage=wgpu.TextureUsage.COPY_DST | wgpu.TextureUsage.COPY_SRC
                )

                # Upload text to GPU
                gpu.device.queue.write_texture(
                    {"texture": text_texture},
                    text_bytes,
                    {"bytes_per_row": text_width * 4, "rows_per_image": text_height},
                    (text_width, text_height, 1)
                )

                # Copy text texture onto output texture at label position
                # Ensure position is within bounds
                label_x = max(0, min(label_x, width - text_width))
                label_y = max(0, min(label_y, height - text_height))

                copy_encoder = gpu.device.create_command_encoder()
                copy_encoder.copy_texture_to_texture(
                    {"texture": text_texture},
                    {"texture": output_texture, "origin": (label_x, label_y, 0)},
                    (text_width, text_height, 1)
                )
                gpu.queue.submit([copy_encoder.finish()])

                # Cleanup
                text_texture.destroy()

            # Create output frame
            output_frame = frame.clone_with_texture(output_texture)

            # Log
            if not hasattr(draw_detections, 'last_log'):
                draw_detections.last_log = 0
            if tick.frame_number - draw_detections.last_log > 30:
                names = ', '.join(set(detections['names']))
                print(f"[Compositor] Drew {len(detections['boxes'][:32])} boxes (GPU): {names}")
                draw_detections.last_log = tick.frame_number

            # Cleanup temp buffers
            box_buffer.destroy()
            num_boxes_buffer.destroy()
        else:
            output_frame = frame
    else:
        output_frame = frame

    # Always output video (never block!)
    outputs['video'].write(output_frame)


# ============================================================================
# Main Example
# ============================================================================

async def main():
    """
    Run object detection pipeline with real-time compositing.
    """
    print("=" * 80)
    print("Object Detection Example")
    print("=" * 80)
    print()
    print("Pipeline:")
    print("  Camera (30 FPS) → ML Detector (10-20 FPS) → Compositor → Display")
    print()
    print("The video pipeline runs at full 30 FPS while ML processes")
    print("independently. Boxes appear as soon as first detection completes.")
    print()
    print("Press Ctrl+C to stop")
    print("=" * 80)
    print()

    # Create runtime at 30 FPS
    runtime = StreamRuntime(fps=30, width=1920, height=1080)

    # Create handlers using decorators
    @camera_source(device_id='0x1424001bcf2284')  # Live Camera
    def camera():
        """Zero-copy camera capture"""
        pass

    @display_sink(title="Object Detection", show_fps=True)
    def display():
        """Display with FPS counter"""
        pass

    # Add streams to runtime
    runtime.add_stream(Stream(camera))
    runtime.add_stream(Stream(object_detector))
    runtime.add_stream(Stream(draw_detections))
    runtime.add_stream(Stream(display))

    # Connect the pipeline
    # Camera → ML Detector (for inference)
    runtime.connect(camera.outputs['video'], object_detector.inputs['video'])

    # Camera → Compositor (main video path)
    runtime.connect(camera.outputs['video'], draw_detections.inputs['video'])

    # ML Detector → Compositor (detection data)
    runtime.connect(
        object_detector.outputs['detections'],
        draw_detections.inputs['detections']
    )

    # Compositor → Display (final output)
    runtime.connect(draw_detections.outputs['video'], display.inputs['video'])

    print("[Runtime] Starting pipeline...")
    print()

    # Run until interrupted
    try:
        await runtime.run()
    except KeyboardInterrupt:
        print("\n[Runtime] Stopping...")
        await runtime.stop()
        print("[Runtime] Stopped")


if __name__ == "__main__":
    asyncio.run(main())
