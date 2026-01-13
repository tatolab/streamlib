# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib subprocess runner for isolated Python processors.

This module is invoked by the Rust host to run Python processors in a
subprocess with full dependency isolation.

Usage (from Rust host):
    python -m streamlib._subprocess_runner \
        --control-socket /tmp/streamlib-xxx-control.sock \
        --frames-socket /tmp/streamlib-xxx-frames.sock \
        --processor-name MyProcessor \
        --project-path /path/to/project
"""

import argparse
import importlib.util
import json
import logging
import os
import socket
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional

# Set up logging
logging.basicConfig(
    level=logging.DEBUG,
    format="[%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("streamlib.subprocess")


@dataclass
class PortSpec:
    """Port specification from host."""
    name: str
    direction: str  # "Input" or "Output"
    schema_name: str
    transport: str  # "Gpu" or "Cpu"


@dataclass
class FrameMessage:
    """Frame message for IPC."""
    direction: str  # "ToGuest" or "ToHost"
    port: str
    schema_name: str
    transport: dict  # {"GpuXpc": {"xpc_object_id": ...}} or {"Shm": {...}}
    timestamp_ns: int
    frame_number: int
    metadata: dict


class IpcChannel:
    """IPC channel for communication with host."""

    def __init__(self, socket_path: str):
        self.socket_path = socket_path
        self.sock: Optional[socket.socket] = None
        self.buffer = b""

    def connect(self):
        """Connect to the host socket."""
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(self.socket_path)
        logger.info(f"Connected to {self.socket_path}")

    def send(self, message: dict):
        """Send a JSON message."""
        data = json.dumps(message) + "\n"
        self.sock.sendall(data.encode("utf-8"))

    def recv(self) -> dict:
        """Receive a JSON message."""
        while b"\n" not in self.buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("Connection closed by host")
            self.buffer += chunk

        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line.decode("utf-8"))

    def recv_nonblocking(self) -> Optional[dict]:
        """Receive a JSON message without blocking.

        Returns:
            message_dict or None if no complete message available
        """
        # Check if we already have a complete message in buffer
        if b"\n" in self.buffer:
            line, self.buffer = self.buffer.split(b"\n", 1)
            message = json.loads(line.decode("utf-8"))
            logger.debug(f"[IPC RECV] found message in buffer (pid={os.getpid()})")
            return message

        # Try to receive more data (non-blocking via select)
        import select
        readable, _, _ = select.select([self.sock], [], [], 0.001)
        if not readable:
            return None

        try:
            chunk = self.sock.recv(4096)
            if not chunk:
                return None
            self.buffer += chunk
        except BlockingIOError:
            return None

        # Check again for complete message
        if b"\n" not in self.buffer:
            return None

        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line.decode("utf-8"))

    def close(self):
        """Close the connection."""
        if self.sock:
            self.sock.close()
            self.sock = None


class SubprocessTimeContext:
    """Time context for subprocess mode.

    Provides timing information similar to the host TimeContext.
    Uses the subprocess start time as the reference point.
    """

    def __init__(self):
        import time
        self._start_time = time.monotonic()

    @property
    def elapsed_secs(self) -> float:
        """Seconds since the subprocess started."""
        import time
        return time.monotonic() - self._start_time

    @property
    def elapsed_ns(self) -> int:
        """Nanoseconds since the subprocess started."""
        return int(self.elapsed_secs * 1_000_000_000)

    @property
    def now_ns(self) -> int:
        """Raw monotonic clock value in nanoseconds."""
        import time
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

        Creates a new buffer. For output back to host, the host must
        import via XPC or shared memory.

        Args:
            width: Buffer width in pixels
            height: Buffer height in pixels
            format: Pixel format string: "bgra32", "rgba32", etc.

        Returns:
            PixelBuffer ready for rendering
        """
        if self._PixelBuffer is None:
            try:
                from streamlib._native import PixelBuffer
                self._PixelBuffer = PixelBuffer
            except ImportError as e:
                raise RuntimeError(f"PixelBuffer not available in native module: {e}")

        return self._PixelBuffer(width, height, format)

    def gl_context(self):
        """Get or create a local RHI OpenGL context.

        Returns a GlContext from the Rust RHI (via PyO3 bindings).
        This provides full RHI functionality including:
        - make_current() / clear_current()
        - create_texture_binding()
        - IOSurface interop on macOS

        This is the escape hatch for when you need direct OpenGL access
        (e.g., for Skia rendering). Most code should use RHI abstractions.
        """
        if self._gl_ctx is None:
            try:
                # Import from internal native module (not public API)
                from streamlib._native import GlContext
                self._gl_ctx = GlContext()
                logger.info("Created RHI GlContext for subprocess")
            except ImportError as e:
                logger.warning(f"streamlib._native.GlContext not available: {e}")
                self._gl_ctx = None
            except Exception as e:
                logger.warning(f"Failed to create RHI GlContext: {e}")
                self._gl_ctx = None
        return self._gl_ctx


class SubprocessProcessorContext:
    """ProcessorContext implementation for subprocess mode.

    Provides the same API as the embedded ProcessorContext but communicates
    via IPC with the host process. GPU frames are shared via XPC on macOS.
    """

    def __init__(self, control: IpcChannel, frames: IpcChannel, config: dict, ports: List[PortSpec]):
        self.control = control
        self.frames = frames
        self.config = config
        self.ports = {p.name: p for p in ports}
        self._input_frames: Dict[str, Any] = {}  # port_name -> imported PixelBuffer
        self._output_frames: Dict[str, Any] = {}  # port_name -> (PixelBuffer, metadata)
        self._frame_number = 0
        self.gpu = SubprocessGpuContext()
        self.time = SubprocessTimeContext()
        self._PixelBuffer = None  # Lazy import
        # XPC frame channel for GPU frame sharing (macOS only)
        self._xpc_channel = None
        self._xpc_service_name: Optional[str] = None

    def _get_pixel_buffer_class(self):
        """Lazy import of PixelBuffer class."""
        if self._PixelBuffer is None:
            from streamlib._native import PixelBuffer
            self._PixelBuffer = PixelBuffer
        return self._PixelBuffer

    def connect_xpc(self, service_name: str):
        """Connect to XPC channel for GPU frame sharing.

        The Rust host creates the XPC listener and registers with the broker.
        The subprocess connects via the broker to establish a direct connection.

        Called after receiving Initialize with xpc_service_name.
        """
        if service_name is None:
            logger.debug("No XPC service name provided, skipping XPC connection")
            return

        try:
            from streamlib._native import XpcFrameChannel
            self._xpc_channel = XpcFrameChannel.connect(service_name)
            self._xpc_service_name = service_name
            logger.info(f"Connected to XPC channel: {service_name} (pid={os.getpid()})")
        except ImportError as e:
            logger.warning(f"XpcFrameChannel not available: {e}")
        except Exception as e:
            logger.error(f"Failed to connect to XPC channel '{service_name}': {e}", exc_info=True)

    def input(self, port_name: str) -> "InputPortProxy":
        """Get input port proxy."""
        return InputPortProxy(self, port_name)

    def output(self, port_name: str) -> "OutputPortProxy":
        """Get output port proxy."""
        return OutputPortProxy(self, port_name)

    def receive_frames_from_host(self):
        """Receive any pending input frames from host before process()."""
        import select
        logger.debug("receive_frames_from_host: starting")
        frames_received = 0
        try:
            while True:
                # Use select with short timeout to check for data
                readable, _, _ = select.select([self.frames.sock], [], [], 0.01)
                if not readable:
                    logger.debug(f"receive_frames_from_host: select says no data, received {frames_received} frames")
                    break

                # Data is available, receive it
                try:
                    msg = self.frames.recv_nonblocking()

                    if msg is None:
                        logger.debug(f"receive_frames_from_host: recv returned None, received {frames_received} frames")
                        break

                    if msg.get("direction") == "ToGuest":
                        logger.debug(f"receive_frames_from_host: got ToGuest frame for port {msg.get('port')}")
                        self._import_input_frame(msg)
                        frames_received += 1
                except BlockingIOError:
                    logger.debug(f"receive_frames_from_host: socket would block, received {frames_received} frames")
                    break
                except Exception as e:
                    logger.warning(f"Error receiving frame: {e}", exc_info=True)
                    break
        finally:
            pass  # Socket stays in default blocking mode

    def _import_input_frame(self, msg: dict):
        """Import an input frame from IPC message.

        Args:
            msg: Frame message dict
        """
        port = msg["port"]
        transport = msg.get("transport", {})
        metadata = msg.get("metadata", {})
        frame_number = msg.get("frame_number", 0)

        logger.debug(
            f"[FRAME IMPORT] frame={frame_number} port={port} transport={list(transport.keys())} "
            f"(pid={os.getpid()})"
        )

        if "GpuXpc" in transport:
            # XPC-based GPU transport - IOSurface shared via XPC connection
            xpc_data = transport["GpuXpc"]
            xpc_object_id = xpc_data["xpc_object_id"]
            width = metadata.get("width", 1920)
            height = metadata.get("height", 1080)
            format_str = metadata.get("format", "Bgra32").lower()

            logger.debug(
                f"[XPC IMPORT] frame={frame_number} xpc_object_id={xpc_object_id} "
                f"{width}x{height} {format_str} (pid={os.getpid()})"
            )

            if self._xpc_channel is None:
                logger.error(
                    f"[XPC IMPORT] FAILED: frame={frame_number} "
                    f"No XPC channel connected (pid={os.getpid()})"
                )
            else:
                try:
                    buffer = self._xpc_channel.import_iosurface(
                        xpc_object_id, width, height, format_str
                    )
                    self._input_frames[port] = {
                        "buffer": buffer,
                        "timestamp_ns": msg.get("timestamp_ns", 0),
                        "frame_number": frame_number,
                        "width": width,
                        "height": height,
                        "format": format_str,
                        "xpc_object_id": xpc_object_id,  # Store for acknowledgment
                    }
                    logger.info(
                        f"[XPC IMPORT] SUCCESS: frame={frame_number} "
                        f"xpc_object_id={xpc_object_id} {width}x{height} (pid={os.getpid()})"
                    )
                except Exception as e:
                    logger.error(
                        f"[XPC IMPORT] FAILED: frame={frame_number} "
                        f"xpc_object_id={xpc_object_id} error={e} (pid={os.getpid()})",
                        exc_info=True
                    )
        elif "Gpu" in transport:
            # Legacy GPU transport with IOSurface ID (kIOSurfaceIsGlobal)
            handle_data = transport["Gpu"]["handle"]
            width = metadata.get("width", 1920)
            height = metadata.get("height", 1080)
            format_str = metadata.get("format", "Bgra32").lower()

            logger.debug(
                f"[IOSURFACE IMPORT] GPU transport: handle_data={handle_data} "
                f"{width}x{height} {format_str} (pid={os.getpid()})"
            )

            try:
                PixelBuffer = self._get_pixel_buffer_class()

                if "IOSurface" in handle_data:
                    iosurface_id = handle_data["IOSurface"]["id"]
                    logger.debug(
                        f"[IOSURFACE IMPORT] ID-based: frame={frame_number} "
                        f"id={iosurface_id} {width}x{height} {format_str} (pid={os.getpid()})"
                    )
                    buffer = PixelBuffer.from_iosurface_id(iosurface_id, width, height, format_str)
                    self._input_frames[port] = {
                        "buffer": buffer,
                        "timestamp_ns": msg.get("timestamp_ns", 0),
                        "frame_number": frame_number,
                        "width": width,
                        "height": height,
                        "format": format_str,
                    }
                    logger.info(
                        f"[IOSURFACE IMPORT] ID-based SUCCESS: frame={frame_number} "
                        f"port='{port}' {width}x{height} ID={iosurface_id} (pid={os.getpid()})"
                    )
                else:
                    logger.error(
                        f"[IOSURFACE IMPORT] FAILED: frame={frame_number} "
                        f"Unknown GPU handle type: {handle_data} (pid={os.getpid()})"
                    )
            except Exception as e:
                logger.error(
                    f"[IOSURFACE IMPORT] FAILED: frame={frame_number} "
                    f"Exception: {e} (pid={os.getpid()})",
                    exc_info=True
                )
        else:
            logger.warning(f"Unsupported transport for input frame: {transport}")

    def get_input_frame(self, port_name: str) -> Optional[Dict]:
        """Get the latest input frame for a port."""
        return self._input_frames.get(port_name)

    def set_output_frame(self, port_name: str, buffer, timestamp_ns: Optional[int] = None):
        """Set output frame to send to host.

        Args:
            port_name: Output port name
            buffer: PixelBuffer containing rendered output
            timestamp_ns: Optional frame timestamp
        """
        self._output_frames[port_name] = {
            "buffer": buffer,
            "timestamp_ns": timestamp_ns or int(self.time.now_ns),
        }

    def send_output_frames_to_host(self):
        """Send any pending output frames to host after process()."""
        for port_name, frame_data in self._output_frames.items():
            buffer = frame_data["buffer"]
            timestamp_ns = frame_data["timestamp_ns"]

            try:
                if self._xpc_channel is not None:
                    # Use XPC for output (faster, bidirectional)
                    xpc_object_id = self._xpc_channel.export_iosurface(buffer)
                    msg = {
                        "direction": "ToHost",
                        "port": port_name,
                        "schema_name": "VideoFrame",
                        "transport": {
                            "GpuXpc": {"xpc_object_id": xpc_object_id}
                        },
                        "timestamp_ns": timestamp_ns,
                        "frame_number": self._frame_number,
                        "metadata": {
                            "width": buffer.width,
                            "height": buffer.height,
                            "format": str(buffer.format),
                        },
                    }
                    self.frames.send(msg)
                    logger.debug(
                        f"Sent output frame on port '{port_name}': XPC object_id={xpc_object_id}"
                    )
                else:
                    # Fallback to IOSurface ID (legacy, requires kIOSurfaceIsGlobal)
                    iosurface_id = buffer.iosurface_id()
                    msg = {
                        "direction": "ToHost",
                        "port": port_name,
                        "schema_name": "VideoFrame",
                        "transport": {
                            "Gpu": {
                                "handle": {
                                    "IOSurface": {"id": iosurface_id}
                                }
                            }
                        },
                        "timestamp_ns": timestamp_ns,
                        "frame_number": self._frame_number,
                        "metadata": {
                            "width": buffer.width,
                            "height": buffer.height,
                            "format": str(buffer.format),
                        },
                    }
                    self.frames.send(msg)
                    logger.debug(
                        f"Sent output frame on port '{port_name}': IOSurface id={iosurface_id}"
                    )
            except Exception as e:
                logger.error(f"Failed to send output frame on port '{port_name}': {e}", exc_info=True)

        self._output_frames.clear()
        self._frame_number += 1


class InputPortProxy:
    """Proxy for input port access."""

    def __init__(self, ctx: SubprocessProcessorContext, port_name: str):
        self.ctx = ctx
        self.port_name = port_name

    def get(self) -> Optional[Any]:
        """Get the latest frame from this input."""
        frame_data = self.ctx.get_input_frame(self.port_name)
        if frame_data is None:
            return None
        # Return the PixelBuffer from the frame data
        return frame_data.get("buffer")


class OutputPortProxy:
    """Proxy for output port access."""

    def __init__(self, ctx: SubprocessProcessorContext, port_name: str):
        self.ctx = ctx
        self.port_name = port_name

    def set(self, value: Any):
        """Set the output value.

        Accepts either:
        - A dict with "pixel_buffer" key (legacy embedded API format)
        - A PixelBuffer directly

        Args:
            value: PixelBuffer or dict with pixel_buffer key
        """
        if isinstance(value, dict):
            # Legacy format: {"pixel_buffer": buffer, "timestamp_ns": ..., ...}
            buffer = value.get("pixel_buffer")
            timestamp_ns = value.get("timestamp_ns")
            self.ctx.set_output_frame(self.port_name, buffer, timestamp_ns)
        else:
            # Direct PixelBuffer
            self.ctx.set_output_frame(self.port_name, value)


def load_processor_class(project_path: Path, entry_point: Optional[str], class_name: str) -> type:
    """Load the processor class from the project."""
    # Add project path to sys.path
    sys.path.insert(0, str(project_path))

    if entry_point:
        # Load from specific file
        module_path = project_path / entry_point
        spec = importlib.util.spec_from_file_location("processor_module", module_path)
        if spec is None or spec.loader is None:
            raise ImportError(f"Cannot load module from {module_path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    else:
        # Try to find the class in the project's main module
        raise NotImplementedError("Auto-discovery not implemented, entry_point required")

    # Get the processor class
    if not hasattr(module, class_name):
        raise AttributeError(f"Module does not have class '{class_name}'")

    return getattr(module, class_name)


def run_subprocess(args: argparse.Namespace):
    """Main subprocess runner loop."""
    logger.info(f"Starting subprocess runner for {args.processor_name} (pid={os.getpid()})")
    logger.info(f"Project path: {args.project_path}")

    # Connect to IPC sockets
    control = IpcChannel(args.control_socket)
    frames = IpcChannel(args.frames_socket)

    try:
        logger.info(f"Connecting to control socket: {args.control_socket}")
        control.connect()
        logger.info(f"Control socket connected, fd={control.sock.fileno()}")
        logger.info(f"Connecting to frames socket: {args.frames_socket}")
        frames.connect()
        logger.info(f"Frames socket connected, fd={frames.sock.fileno()}")
        logger.info("Both sockets connected")

        processor_instance = None
        ctx = None

        while True:
            logger.debug("Waiting for control message...")
            message = control.recv()
            logger.debug(f"Received control message: {list(message.keys()) if isinstance(message, dict) else message}")
            msg_type = next(iter(message.keys())) if isinstance(message, dict) else message

            if msg_type == "Initialize":
                data = message["Initialize"]
                config = data["config"]
                xpc_service_name = data.get("xpc_service_name")
                ports = [
                    PortSpec(
                        name=p["name"],
                        direction=p["direction"],
                        schema_name=p["schema"]["name"],
                        transport=p["schema"]["transport"],
                    )
                    for p in data["ports"]
                ]

                logger.info(f"Initializing with config: {config}")
                logger.info(f"Ports: {[p.name for p in ports]}")
                if xpc_service_name:
                    logger.info(f"XPC service name: {xpc_service_name}")

                # Load and instantiate processor
                processor_class = load_processor_class(
                    Path(args.project_path),
                    config.get("entry_point"),
                    config.get("class_name", args.processor_name),
                )
                processor_instance = processor_class()

                # Create context
                ctx = SubprocessProcessorContext(control, frames, config, ports)

                # Connect to XPC service for GPU frame sharing (macOS only)
                if xpc_service_name:
                    ctx.connect_xpc(xpc_service_name)

                # Call setup if available
                if hasattr(processor_instance, "setup"):
                    processor_instance.setup(ctx)

                # Send Ready
                control.send({
                    "Ready": {
                        "metadata": {
                            "name": args.processor_name,
                            "descriptor": None,
                        }
                    }
                })

            elif msg_type == "Setup":
                data = message["Setup"]
                shm_regions = data["shm_regions"]
                logger.info(f"Setting up {len(shm_regions)} shared memory regions")

                # TODO: Open shared memory regions for CPU data (audio, dataframes)

                control.send("SetupComplete")

            elif msg_type == "Process":
                logger.debug(f"[{args.processor_name}] Handling Process message")
                if processor_instance is None or ctx is None:
                    logger.error(f"[{args.processor_name}] Process called but processor not initialized")
                    control.send({"Error": {"message": "Processor not initialized"}})
                    continue

                try:
                    # Receive input frames from host before processing
                    logger.debug(f"[{args.processor_name}] Receiving input frames from host...")
                    ctx.receive_frames_from_host()
                    logger.debug(f"[{args.processor_name}] Input frames received: {list(ctx._input_frames.keys())}")

                    # Call processor's process method
                    if hasattr(processor_instance, "process"):
                        logger.debug("Calling processor.process(ctx)...")
                        processor_instance.process(ctx)
                        logger.debug("processor.process(ctx) completed")

                    # Send output frames to host after processing
                    logger.debug(f"Sending output frames: {list(ctx._output_frames.keys())}")
                    ctx.send_output_frames_to_host()
                    logger.debug("Output frames sent")

                    control.send("ProcessComplete")
                    logger.debug("Sent ProcessComplete")
                except Exception as e:
                    logger.exception("Error in process()")
                    control.send({"Error": {"message": str(e)}})

            elif msg_type == "Pause":
                logger.debug("Received Pause")
                if processor_instance and hasattr(processor_instance, "on_pause"):
                    processor_instance.on_pause()

            elif msg_type == "Resume":
                logger.debug("Received Resume")
                if processor_instance and hasattr(processor_instance, "on_resume"):
                    processor_instance.on_resume()

            elif msg_type == "Teardown":
                logger.info("Received Teardown")
                if processor_instance and hasattr(processor_instance, "teardown"):
                    processor_instance.teardown(ctx)

            elif msg_type == "Shutdown":
                logger.info("Received Shutdown, exiting")
                break

            else:
                logger.warning(f"Unknown message type: {msg_type}")

    except Exception as e:
        logger.exception("Subprocess runner error")
        raise
    finally:
        control.close()
        frames.close()


def main():
    parser = argparse.ArgumentParser(description="StreamLib Python subprocess runner")
    parser.add_argument("--control-socket", required=True, help="Control IPC socket path")
    parser.add_argument("--frames-socket", required=True, help="Frames IPC socket path")
    parser.add_argument("--processor-name", required=True, help="Processor name")
    parser.add_argument("--project-path", required=True, help="Project path")
    args = parser.parse_args()

    run_subprocess(args)


if __name__ == "__main__":
    main()
