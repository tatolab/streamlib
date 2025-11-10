"""
Example Python processor using @StreamProcessor decorator pattern.

This demonstrates the decorator-based port declaration system that mirrors
Rust's #[input] and #[output] attributes.
"""

from streamlib import StreamProcessor, input_port, output_port, VideoFrame


@StreamProcessor
class EdgeDetectorProcessor:
    """
    Example processor that detects edges in video frames using OpenCV.

    Demonstrates decorator-based port declaration compatible with Rust introspection.
    """

    @input_port(name="video", type=VideoFrame, required=True)
    def video(self):
        """Video input port - receives frames for processing"""
        return self._video_in

    @output_port(name="edges", type=VideoFrame)
    def edges(self):
        """Edge detection output port - produces processed frames"""
        return self._video_out

    def process(self, tick):
        """
        Process one frame: read from input, detect edges, write to output.

        Args:
            tick: TimedTick object with frame_number, timestamp, delta_time
        """
        # Read from input port
        frame = self.input_ports().video.read_latest()

        if frame is None:
            return  # No data available yet

        # TODO: Process frame with OpenCV edge detection
        # For now, just pass through
        processed_frame = frame

        # Write to output port
        self.output_ports().edges.write(processed_frame)


@StreamProcessor
class VideoPassthroughProcessor:
    """
    Simple passthrough processor for testing.

    Demonstrates minimal decorator usage.
    """

    @input_port(name="input", type=VideoFrame)
    def input(self):
        return self._input

    @output_port(name="output", type=VideoFrame)
    def output(self):
        return self._output

    def process(self, tick):
        frame = self.input_ports().input.read_latest()
        if frame:
            self.output_ports().output.write(frame)


if __name__ == "__main__":
    # Demonstrate metadata introspection
    print("EdgeDetectorProcessor metadata:")
    print(f"  Inputs: {EdgeDetectorProcessor.__streamlib_inputs__}")
    print(f"  Outputs: {EdgeDetectorProcessor.__streamlib_outputs__}")

    print("\nVideoPassthroughProcessor metadata:")
    print(f"  Inputs: {VideoPassthroughProcessor.__streamlib_inputs__}")
    print(f"  Outputs: {VideoPassthroughProcessor.__streamlib_outputs__}")
