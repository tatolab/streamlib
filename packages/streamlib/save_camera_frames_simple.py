"""Capture and save camera frames as PNG - simplified version using OpenCV."""
import cv2
import time

def main():
    print("Opening camera...")
    cap = cv2.VideoCapture(0)  # 0 = default camera

    if not cap.isOpened():
        print("❌ Failed to open camera")
        return

    # Set resolution
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, 1920)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, 1080)

    print(f"Camera opened: {int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))}x{int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))}")
    print("Warming up...")
    time.sleep(1)

    print("Capturing frames...")
    for i in range(5):
        ret, frame = cap.read()
        if not ret:
            print(f"❌ Failed to read frame {i+1}")
            continue

        # Convert BGR to RGB
        frame_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)

        # Save as PNG
        filename = f"camera_frame_{i+1}.png"
        # cv2.imwrite expects BGR, so convert back
        cv2.imwrite(filename, frame)

        print(f"✅ Saved {filename} ({frame.shape[1]}x{frame.shape[0]})")
        time.sleep(0.2)

    cap.release()
    print("\n✅ All frames saved!")

if __name__ == "__main__":
    main()
