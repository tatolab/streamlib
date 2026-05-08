# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Capture a specific X11 window by ID to a PNG file.

Usage: capture_window.py <window_id> <output.png>

Uses xwd to capture, then PIL to convert XWD → PNG.
"""
import subprocess
import sys
import tempfile
import os

def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <window_id> <output.png>", file=sys.stderr)
        sys.exit(1)

    window_id = sys.argv[1]
    output_path = sys.argv[2]

    # Capture with xwd
    with tempfile.NamedTemporaryFile(suffix=".xwd", delete=False) as tmp:
        tmp_path = tmp.name

    try:
        subprocess.run(
            ["xwd", "-id", window_id, "-silent"],
            stdout=open(tmp_path, "wb"),
            stderr=subprocess.DEVNULL,
            check=True,
        )

        # Convert XWD to PNG via PIL
        from PIL import Image
        img = Image.open(tmp_path)
        img.save(output_path, "PNG")
        print(f"OK: {output_path} ({os.path.getsize(output_path)} bytes, {img.size[0]}x{img.size[1]})")
    except subprocess.CalledProcessError:
        print(f"FAIL: xwd capture failed for window {window_id}", file=sys.stderr)
        sys.exit(1)
    except Exception as e:
        print(f"FAIL: {e}", file=sys.stderr)
        sys.exit(1)
    finally:
        os.unlink(tmp_path)

if __name__ == "__main__":
    main()
