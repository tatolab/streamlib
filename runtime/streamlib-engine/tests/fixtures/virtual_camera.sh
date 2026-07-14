#!/bin/bash
# Virtual camera fixture for E2E tests.
#
# Usage:
#   ./virtual_camera.sh start [device_path]  — start streaming test pattern
#   ./virtual_camera.sh stop                 — stop streaming
#   ./virtual_camera.sh check                — check if v4l2loopback is available
#
# Requires: v4l2loopback kernel module loaded, ffmpeg installed.
# Load module: sudo modprobe v4l2loopback video_nr=10 card_label="Virtual_Camera" exclusive_caps=1

DEVICE="${2:-/dev/video10}"
PID_FILE="/tmp/streamlib-virtual-camera.pid"

case "$1" in
    check)
        if [ ! -e "$DEVICE" ]; then
            echo "UNAVAILABLE: $DEVICE does not exist. Load v4l2loopback first."
            exit 1
        fi
        if ! command -v ffmpeg &>/dev/null; then
            echo "UNAVAILABLE: ffmpeg not found"
            exit 1
        fi
        echo "AVAILABLE: $DEVICE"
        exit 0
        ;;
    start)
        if [ ! -e "$DEVICE" ]; then
            echo "ERROR: $DEVICE does not exist" >&2
            exit 1
        fi
        # Kill any existing stream
        if [ -f "$PID_FILE" ]; then
            kill "$(cat "$PID_FILE")" 2>/dev/null
            rm -f "$PID_FILE"
        fi
        # Stream animated test pattern at 30fps 1920x1080 YUYV
        ffmpeg -f lavfi -i "testsrc=duration=300:size=1920x1080:rate=30" \
            -pix_fmt yuyv422 -f v4l2 "$DEVICE" \
            -loglevel error </dev/null &>/dev/null &
        echo $! > "$PID_FILE"
        # Wait for ffmpeg to start producing frames
        sleep 1
        echo "STARTED: PID=$(cat "$PID_FILE") device=$DEVICE"
        exit 0
        ;;
    stop)
        if [ -f "$PID_FILE" ]; then
            kill "$(cat "$PID_FILE")" 2>/dev/null
            rm -f "$PID_FILE"
            echo "STOPPED"
        else
            echo "NOT_RUNNING"
        fi
        exit 0
        ;;
    *)
        echo "Usage: $0 {check|start|stop} [device_path]" >&2
        exit 1
        ;;
esac
