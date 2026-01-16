# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib subprocess runner for isolated Python processors.

This module is invoked by the Rust host to run Python processors in a
subprocess with full dependency isolation.

Architecture (Phase 4):
- Uses STREAMLIB_CONNECTION_ID and STREAMLIB_BROKER_ENDPOINT env vars
- Calls gRPC broker APIs for signaling (ClientAlive, GetHostStatus, MarkAcked)
- Uses XPC via PyO3 wheel bindings for frame transport
- No Unix sockets - all IPC via XPC

Execution Modes:
- Manual: Wait for start() command, then produce/process
- Reactive: Process frames as they arrive via XPC
- Continuous: Run processing loop, send/receive continuously

Usage (from Rust host):
    python -m streamlib._subprocess_runner \
        --class-name MyProcessor \
        --execution-mode continuous  # or manual/reactive
"""

import argparse
import importlib.util
import logging
import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

import grpc

from streamlib._generated import broker_pb2, broker_pb2_grpc

# Set up logging
logging.basicConfig(
    level=logging.DEBUG,
    format="[%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("streamlib.subprocess")

# ACK protocol magic bytes
ACK_PING_MAGIC = bytes([0x53, 0x4C, 0x50])  # "SLP" = StreamLib Ping
ACK_PONG_MAGIC = bytes([0x53, 0x4C, 0x41])  # "SLA" = StreamLib Ack


@dataclass
class SubprocessConfig:
    """Configuration for the subprocess."""

    connection_id: str
    broker_endpoint: str
    class_name: str
    execution_mode: str  # "manual", "reactive", "continuous"
    project_path: Path


class SubprocessTimeContext:
    """Time context for subprocess mode.

    Provides timing information similar to the host TimeContext.
    Uses the subprocess start time as the reference point.
    """

    def __init__(self):
        self._start_time = time.monotonic()

    @property
    def elapsed_secs(self) -> float:
        """Seconds since the subprocess started."""
        return time.monotonic() - self._start_time

    @property
    def elapsed_ns(self) -> int:
        """Nanoseconds since the subprocess started."""
        return int(self.elapsed_secs * 1_000_000_000)

    @property
    def now_ns(self) -> int:
        """Raw monotonic clock value in nanoseconds."""
        return int(time.monotonic() * 1_000_000_000)


class SubprocessGpuContext:
    """GPU context for subprocess mode.

    Creates a local RHI OpenGL context in the subprocess. This uses the
    same Rust RHI as embedded mode, but in a separate process.

    IOSurface handles are shared via XPC for zero-copy GPU texture transfer.
    """

    def __init__(self):
        self._gl_ctx = None
        self._PixelBuffer = None  # Lazy import

    def acquire_pixel_buffer(self, width: int, height: int, format: str):
        """Acquire a pixel buffer for rendering.

        Args:
            width: Buffer width in pixels
            height: Buffer height in pixels
            format: Pixel format string: "bgra32", "rgba32", etc.

        Returns:
            PixelBuffer ready for rendering
        """
        if self._PixelBuffer is None:
            try:
                from streamlib._native import PyRhiPixelBuffer as PixelBuffer

                self._PixelBuffer = PixelBuffer
            except ImportError as e:
                raise RuntimeError(f"PixelBuffer not available in native module: {e}")

        return self._PixelBuffer(width, height, format)

    def gl_context(self):
        """Get or create a local RHI OpenGL context.

        Returns a GlContext from the Rust RHI (via PyO3 bindings).
        """
        if self._gl_ctx is None:
            try:
                from streamlib._native import PyGlContext as GlContext

                self._gl_ctx = GlContext()
                logger.info("Created RHI GlContext for subprocess")
            except ImportError as e:
                logger.warning(f"streamlib._native.PyGlContext not available: {e}")
                self._gl_ctx = None
            except Exception as e:
                logger.warning(f"Failed to create RHI GlContext: {e}")
                self._gl_ctx = None
        return self._gl_ctx


class SubprocessProcessorContext:
    """ProcessorContext implementation for subprocess mode.

    Provides the same API as the embedded ProcessorContext but communicates
    via XPC with the host process. GPU frames are shared via XPC on macOS.
    """

    def __init__(self, xpc_connection, config: dict):
        self._xpc_connection = xpc_connection
        self.config = config
        self._frame_number = 0
        self.gpu = SubprocessGpuContext()
        self.time = SubprocessTimeContext()
        self._input_frames: dict = {}
        self._output_frames: dict = {}

    def input(self, port_name: str) -> "InputPortProxy":
        """Get input port proxy."""
        return InputPortProxy(self, port_name)

    def output(self, port_name: str) -> "OutputPortProxy":
        """Get output port proxy."""
        return OutputPortProxy(self, port_name)

    def receive_frames_from_host(self):
        """Receive any pending input frames from host via XPC."""
        if self._xpc_connection is None:
            return

        # Try to receive frames from all input ports via XPC
        try:
            # Use PyO3 XPC binding to receive frames
            # Returns tuple of (port_name, frame_id, pixel_buffer) or None
            while True:
                result = self._xpc_connection.recv_frame("*", timeout_ms=1)
                if result is None:
                    break
                frame_id, buffer = result
                # Store the received frame (port name extracted from buffer metadata)
                port_name = getattr(buffer, "port_name", "video_in")
                self._input_frames[port_name] = {
                    "buffer": buffer,
                    "frame_number": frame_id,
                    "timestamp_ns": self.time.now_ns,
                }
                logger.debug(f"Received frame {frame_id} on port '{port_name}'")
        except Exception as e:
            logger.warning(f"Error receiving frames: {e}")

    def send_output_frames_to_host(self):
        """Send any pending output frames to host via XPC."""
        if self._xpc_connection is None:
            return

        for port_name, frame_data in self._output_frames.items():
            buffer = frame_data["buffer"]
            try:
                frame_id = self._xpc_connection.send_frame(port_name, buffer)
                logger.debug(f"Sent frame {frame_id} on port '{port_name}'")
            except Exception as e:
                logger.error(f"Failed to send frame on port '{port_name}': {e}")

        self._output_frames.clear()
        self._frame_number += 1

    def get_input_frame(self, port_name: str) -> Optional[dict]:
        """Get the latest input frame for a port."""
        return self._input_frames.get(port_name)

    def set_output_frame(self, port_name: str, buffer, timestamp_ns: Optional[int] = None):
        """Set output frame to send to host."""
        self._output_frames[port_name] = {
            "buffer": buffer,
            "timestamp_ns": timestamp_ns or self.time.now_ns,
        }


class InputPortProxy:
    """Proxy for input port access.

    Transparently deserializes XPC data to Python dict using schema.
    """

    def __init__(self, ctx: SubprocessProcessorContext, port_name: str):
        self.ctx = ctx
        self.port_name = port_name

    def get(self) -> Optional[Any]:
        """Get the latest frame from this input.

        Internally calls XPC receive and deserializes using schema.
        Python processor sees normal dict or PixelBuffer.
        """
        frame_data = self.ctx.get_input_frame(self.port_name)
        if frame_data is None:
            return None
        return frame_data.get("buffer")


class OutputPortProxy:
    """Proxy for output port access.

    Transparently serializes Python dict to XPC using schema.
    """

    def __init__(self, ctx: SubprocessProcessorContext, port_name: str):
        self.ctx = ctx
        self.port_name = port_name

    def set(self, value: Any):
        """Set the output value.

        Takes Python dict or PixelBuffer from processor.
        Serializes to XPC using schema internally.

        Args:
            value: PixelBuffer or dict with pixel_buffer key
        """
        if isinstance(value, dict):
            buffer = value.get("pixel_buffer")
            timestamp_ns = value.get("timestamp_ns")
            self.ctx.set_output_frame(self.port_name, buffer, timestamp_ns)
        else:
            self.ctx.set_output_frame(self.port_name, value)


def load_processor_class(project_path: Path, class_name: str) -> type:
    """Load the processor class from the project."""
    sys.path.insert(0, str(project_path))

    # Look for processor.py or the class in any .py file
    processor_file = project_path / "processor.py"
    if processor_file.exists():
        spec = importlib.util.spec_from_file_location("processor_module", processor_file)
        if spec is None or spec.loader is None:
            raise ImportError(f"Cannot load module from {processor_file}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

        if not hasattr(module, class_name):
            raise AttributeError(f"Module does not have class '{class_name}'")

        return getattr(module, class_name)

    # Try to find by class name in other files
    for py_file in project_path.glob("*.py"):
        if py_file.name.startswith("_"):
            continue
        try:
            spec = importlib.util.spec_from_file_location("candidate_module", py_file)
            if spec is None or spec.loader is None:
                continue
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            if hasattr(module, class_name):
                return getattr(module, class_name)
        except Exception:
            continue

    raise ImportError(f"Could not find class '{class_name}' in {project_path}")


def create_grpc_channel(broker_endpoint: str) -> grpc.Channel:
    """Create a gRPC channel to the broker."""
    return grpc.insecure_channel(broker_endpoint)


def call_client_alive(stub: broker_pb2_grpc.BrokerServiceStub, connection_id: str) -> broker_pb2.ClientAliveResponse:
    """Call ClientAlive gRPC to signal subprocess is alive."""
    request = broker_pb2.ClientAliveRequest(connection_id=connection_id)
    response = stub.ClientAlive(request)
    logger.info(f"ClientAlive response: success={response.success}, host_state={response.host_state}")
    return response


def call_get_host_status(
    stub: broker_pb2_grpc.BrokerServiceStub, connection_id: str
) -> broker_pb2.GetHostStatusResponse:
    """Poll for host status."""
    request = broker_pb2.GetHostStatusRequest(connection_id=connection_id)
    return stub.GetHostStatus(request)


def call_mark_acked(stub: broker_pb2_grpc.BrokerServiceStub, connection_id: str) -> broker_pb2.MarkAckedResponse:
    """Mark client ACK complete."""
    request = broker_pb2.MarkAckedRequest(connection_id=connection_id, side="client")
    response = stub.MarkAcked(request)
    logger.info(f"MarkAcked response: success={response.success}, connection_state={response.connection_state}")
    return response


def wait_for_host_xpc_ready(
    stub: broker_pb2_grpc.BrokerServiceStub, connection_id: str, timeout_secs: float = 30.0
) -> bool:
    """Wait for host to be XPC ready.

    Polls GetHostStatus until host_state is "xpc_ready" or "acked".
    """
    start = time.monotonic()
    poll_interval = 0.1  # 100ms

    while time.monotonic() - start < timeout_secs:
        try:
            response = call_get_host_status(stub, connection_id)
            logger.debug(f"Host state: {response.host_state}")

            # Host is ready if XPC endpoint is stored or already acked
            if response.host_state in ("xpc_ready", "acked"):
                return True
        except grpc.RpcError as e:
            logger.warning(f"GetHostStatus failed: {e}")

        time.sleep(poll_interval)

    logger.error("Timeout waiting for host XPC ready")
    return False


def establish_xpc_connection(connection_id: str):
    """Establish XPC connection to host processor.

    Uses PyO3 XPC bindings to:
    1. Connect to broker XPC service to get host's endpoint
    2. Create XPC connection from the endpoint

    Returns XpcConnection object or None on failure.
    """
    try:
        from streamlib._native import XpcConnection

        # Create connection from env (reads STREAMLIB_CONNECTION_ID internally)
        xpc_conn = XpcConnection.from_env()

        # Connect to host's XPC endpoint via broker
        if not xpc_conn.connect():
            logger.error("Failed to connect to host XPC endpoint")
            return None

        logger.info(f"XPC connection established for {connection_id}")
        return xpc_conn
    except ImportError as e:
        logger.error(f"XpcConnection not available in native module: {e}")
        return None
    except Exception as e:
        logger.error(f"Failed to establish XPC connection: {e}")
        return None


def do_ack_exchange(xpc_connection, timeout_secs: float = 5.0) -> bool:
    """Perform ACK ping/pong exchange with host.

    Protocol:
    1. Wait for ACK ping from host (magic bytes: 0x53 0x4C 0x50 "SLP")
    2. Send ACK pong back (magic bytes: 0x53 0x4C 0x41 "SLA")

    Returns True if exchange succeeded, False on timeout/error.
    """
    if xpc_connection is None:
        logger.error("Cannot do ACK exchange: no XPC connection")
        return False

    try:
        # Wait for ping from host
        timeout_ms = int(timeout_secs * 1000)
        if not xpc_connection.wait_for_ack_ping(timeout_ms):
            logger.error("Timeout waiting for ACK ping from host")
            return False

        logger.debug("Received ACK ping from host")

        # Send pong back
        xpc_connection.send_ack_pong()
        logger.debug("Sent ACK pong to host")

        return True
    except Exception as e:
        logger.error(f"ACK exchange failed: {e}")
        return False


def run_manual_mode(processor_instance, ctx: SubprocessProcessorContext, xpc_connection):
    """Run processor in Manual mode.

    Manual mode: Wait for start() command from host, then begin.
    """
    logger.info("Running in Manual mode - waiting for start() command")

    # Wait for start command via XPC control message
    started = False
    while not started:
        try:
            # Check for control message
            if xpc_connection is not None:
                # Poll for control message (non-blocking)
                # In Phase 4c stub, just check connection state
                if xpc_connection.is_connected():
                    started = True
                    break
            time.sleep(0.1)
        except KeyboardInterrupt:
            logger.info("Interrupted, shutting down")
            return

    logger.info("Start command received, beginning processing")

    # Call start() if processor has it
    if hasattr(processor_instance, "start"):
        processor_instance.start(ctx)

    # For manual generators, they run their own loop in start()
    # For manual processors, they process in start() then return


def run_reactive_mode(processor_instance, ctx: SubprocessProcessorContext, xpc_connection):
    """Run processor in Reactive mode.

    Reactive mode: Process frames as they arrive via XPC.
    """
    logger.info("Running in Reactive mode - processing frames as they arrive")

    while True:
        try:
            # Receive input frames from host
            ctx.receive_frames_from_host()

            # Check if we have any input frames to process
            if ctx._input_frames:
                if hasattr(processor_instance, "process"):
                    processor_instance.process(ctx)

                # Send output frames back
                ctx.send_output_frames_to_host()

                # Clear input frames after processing
                ctx._input_frames.clear()
            else:
                # No frames, sleep briefly
                time.sleep(0.001)

        except KeyboardInterrupt:
            logger.info("Interrupted, shutting down")
            break
        except Exception as e:
            logger.error(f"Error in reactive loop: {e}")
            break


def run_continuous_mode(processor_instance, ctx: SubprocessProcessorContext, xpc_connection):
    """Run processor in Continuous mode.

    Continuous mode: Run processing loop, send/receive frames continuously.
    """
    logger.info("Running in Continuous mode - continuous processing loop")

    while True:
        try:
            # Receive any available input frames
            ctx.receive_frames_from_host()

            # Call process method
            if hasattr(processor_instance, "process"):
                processor_instance.process(ctx)

            # Send output frames
            ctx.send_output_frames_to_host()

            # Continuous processors control their own timing
            # but we add a small yield to prevent tight spinning
            time.sleep(0.001)

        except KeyboardInterrupt:
            logger.info("Interrupted, shutting down")
            break
        except Exception as e:
            logger.error(f"Error in continuous loop: {e}")
            break


def run_subprocess(config: SubprocessConfig):
    """Main subprocess runner."""
    logger.info(f"Starting subprocess runner for {config.class_name} (pid={os.getpid()})")
    logger.info(f"Connection ID: {config.connection_id}")
    logger.info(f"Broker endpoint: {config.broker_endpoint}")
    logger.info(f"Execution mode: {config.execution_mode}")
    logger.info(f"Project path: {config.project_path}")

    # Step 1: Connect to broker via gRPC
    logger.info("Connecting to broker via gRPC...")
    channel = create_grpc_channel(config.broker_endpoint)
    stub = broker_pb2_grpc.BrokerServiceStub(channel)

    # Step 2: Call ClientAlive immediately
    logger.info("Calling ClientAlive...")
    try:
        response = call_client_alive(stub, config.connection_id)
        if not response.success:
            logger.error("ClientAlive failed")
            return
    except grpc.RpcError as e:
        logger.error(f"ClientAlive gRPC error: {e}")
        return

    # Step 3: Wait for host to be XPC ready
    logger.info("Waiting for host XPC ready...")
    if not wait_for_host_xpc_ready(stub, config.connection_id):
        logger.error("Host never became XPC ready")
        return

    # Step 4: Establish XPC connection to host
    logger.info("Establishing XPC connection...")
    xpc_connection = establish_xpc_connection(config.connection_id)
    if xpc_connection is None:
        logger.error("Failed to establish XPC connection")
        return

    # Step 5: ACK exchange
    logger.info("Performing ACK exchange...")
    if not do_ack_exchange(xpc_connection):
        logger.error("ACK exchange failed")
        return

    # Step 6: Mark client as ACKed via gRPC
    logger.info("Calling MarkAcked...")
    try:
        response = call_mark_acked(stub, config.connection_id)
        if not response.success:
            logger.error("MarkAcked failed")
            return
    except grpc.RpcError as e:
        logger.error(f"MarkAcked gRPC error: {e}")
        return

    logger.info("Bridge setup complete, loading processor...")

    # Step 7: Load and instantiate processor
    processor_class = load_processor_class(config.project_path, config.class_name)
    processor_instance = processor_class()
    logger.info(f"Loaded processor: {processor_class.__name__}")

    # Step 8: Create context and call setup
    ctx = SubprocessProcessorContext(xpc_connection, {})

    if hasattr(processor_instance, "setup"):
        processor_instance.setup(ctx)
        logger.info("Processor setup complete")

    # Step 9: Run in the appropriate execution mode
    try:
        if config.execution_mode == "manual":
            run_manual_mode(processor_instance, ctx, xpc_connection)
        elif config.execution_mode == "reactive":
            run_reactive_mode(processor_instance, ctx, xpc_connection)
        elif config.execution_mode == "continuous":
            run_continuous_mode(processor_instance, ctx, xpc_connection)
        else:
            logger.error(f"Unknown execution mode: {config.execution_mode}")
    finally:
        # Cleanup
        if hasattr(processor_instance, "teardown"):
            processor_instance.teardown(ctx)
            logger.info("Processor teardown complete")

        if xpc_connection is not None:
            xpc_connection.close()
            logger.info("XPC connection closed")

        channel.close()
        logger.info("gRPC channel closed")


def main():
    parser = argparse.ArgumentParser(description="StreamLib Python subprocess runner")
    parser.add_argument("--class-name", required=True, help="Processor class name")
    parser.add_argument(
        "--execution-mode",
        choices=["manual", "reactive", "continuous"],
        default="continuous",
        help="Execution mode (default: continuous)",
    )
    args = parser.parse_args()

    # Read required environment variables
    connection_id = os.environ.get("STREAMLIB_CONNECTION_ID")
    if not connection_id:
        logger.error("STREAMLIB_CONNECTION_ID environment variable not set")
        sys.exit(1)

    broker_endpoint = os.environ.get("STREAMLIB_BROKER_ENDPOINT")
    if not broker_endpoint:
        logger.error("STREAMLIB_BROKER_ENDPOINT environment variable not set")
        sys.exit(1)

    # Project path is current directory
    project_path = Path.cwd()

    config = SubprocessConfig(
        connection_id=connection_id,
        broker_endpoint=broker_endpoint,
        class_name=args.class_name,
        execution_mode=args.execution_mode,
        project_path=project_path,
    )

    run_subprocess(config)


if __name__ == "__main__":
    main()
