# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Subprocess processor context — native FFI to iceoryx2 + lifecycle protocol.

Provides the `ctx` object passed to Python processor methods:
  - ctx.inputs.read("port_name") -> deserialized msgpack data or None
  - ctx.outputs.write("port_name", data) -> serializes and sends via iceoryx2
  - ctx.time -> monotonic clock (nanoseconds)
  - ctx.config -> processor config dict

Data I/O uses direct FFI to libstreamlib_python_native via ctypes.
Lifecycle commands (setup/run/stop/teardown) use length-prefixed JSON over pipes.

Protocol (lifecycle only):
  All messages are length-prefixed: [4 bytes u32 BE length][JSON bytes]
"""

import json
import struct
import time

import msgpack


# ============================================================================
# Lifecycle protocol I/O (length-prefixed JSON over stdin/stdout)
# ============================================================================


def bridge_read_message(stdin):
    """Read a length-prefixed JSON message from stdin."""
    len_buf = stdin.read(4)
    if len(len_buf) < 4:
        raise EOFError("stdin closed")
    length = struct.unpack(">I", len_buf)[0]
    msg_buf = stdin.read(length)
    if len(msg_buf) < length:
        raise EOFError("stdin closed mid-message")
    return json.loads(msg_buf)


def bridge_send_message(stdout, msg):
    """Send a length-prefixed JSON message to stdout."""
    json_bytes = json.dumps(msg, separators=(",", ":")).encode("utf-8")
    stdout.write(struct.pack(">I", len(json_bytes)))
    stdout.write(json_bytes)
    stdout.flush()


# ============================================================================
# Native FFI library loading
# ============================================================================


MAX_PAYLOAD_SIZE = 32768


def load_native_lib(lib_path):
    """Load the streamlib-python-native cdylib and configure ctypes signatures."""
    import ctypes

    lib = ctypes.CDLL(lib_path)

    # Context lifecycle
    lib.slpn_context_create.argtypes = [ctypes.c_char_p]
    lib.slpn_context_create.restype = ctypes.c_void_p
    lib.slpn_context_destroy.argtypes = [ctypes.c_void_p]
    lib.slpn_context_destroy.restype = None
    lib.slpn_context_time_ns.argtypes = [ctypes.c_void_p]
    lib.slpn_context_time_ns.restype = ctypes.c_int64

    # Input
    lib.slpn_input_subscribe.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_input_subscribe.restype = ctypes.c_int32
    lib.slpn_input_poll.argtypes = [ctypes.c_void_p]
    lib.slpn_input_poll.restype = ctypes.c_int32
    lib.slpn_input_read.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p,
        ctypes.c_void_p, ctypes.c_uint32,
        ctypes.POINTER(ctypes.c_uint32), ctypes.POINTER(ctypes.c_int64),
    ]
    lib.slpn_input_read.restype = ctypes.c_int32

    # Output
    lib.slpn_output_publish.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p,
        ctypes.c_char_p, ctypes.c_char_p,
    ]
    lib.slpn_output_publish.restype = ctypes.c_int32
    lib.slpn_output_write.argtypes = [
        ctypes.c_void_p, ctypes.c_char_p,
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_int64,
    ]
    lib.slpn_output_write.restype = ctypes.c_int32

    # GPU Surface
    lib.slpn_gpu_surface_lookup.argtypes = [ctypes.c_uint32]
    lib.slpn_gpu_surface_lookup.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_lock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
    lib.slpn_gpu_surface_lock.restype = ctypes.c_int32
    lib.slpn_gpu_surface_unlock.argtypes = [ctypes.c_void_p, ctypes.c_int32]
    lib.slpn_gpu_surface_unlock.restype = ctypes.c_int32
    lib.slpn_gpu_surface_base_address.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_base_address.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_width.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_width.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_height.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_height.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_bytes_per_row.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_bytes_per_row.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_create.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32]
    lib.slpn_gpu_surface_create.restype = ctypes.c_void_p
    lib.slpn_gpu_surface_get_id.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_get_id.restype = ctypes.c_uint32
    lib.slpn_gpu_surface_release.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_release.restype = None
    lib.slpn_gpu_surface_iosurface_ref.argtypes = [ctypes.c_void_p]
    lib.slpn_gpu_surface_iosurface_ref.restype = ctypes.c_void_p

    # Broker
    lib.slpn_broker_connect.argtypes = [ctypes.c_char_p]
    lib.slpn_broker_connect.restype = ctypes.c_void_p
    lib.slpn_broker_disconnect.argtypes = [ctypes.c_void_p]
    lib.slpn_broker_disconnect.restype = None
    lib.slpn_broker_resolve_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_broker_resolve_surface.restype = ctypes.c_void_p
    lib.slpn_broker_acquire_surface.argtypes = [
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32,
        ctypes.c_uint32, ctypes.c_char_p, ctypes.c_uint32,
    ]
    lib.slpn_broker_acquire_surface.restype = ctypes.c_void_p
    lib.slpn_broker_unregister_surface.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    lib.slpn_broker_unregister_surface.restype = None

    return lib


# ============================================================================
# Native FFI context classes
# ============================================================================


class NativeInputs:
    """Input ports backed by iceoryx2 subscribers via FFI."""

    def __init__(self, lib, ctx_ptr):
        import ctypes

        self._lib = lib
        self._ctx_ptr = ctx_ptr
        self._read_buf = (ctypes.c_uint8 * MAX_PAYLOAD_SIZE)()
        self._out_len = ctypes.c_uint32(0)
        self._out_ts = ctypes.c_int64(0)

    def read(self, port_name):
        """Read latest data from a port. Returns deserialized msgpack data or None."""
        import ctypes

        result = self._lib.slpn_input_read(
            self._ctx_ptr,
            port_name.encode("utf-8"),
            ctypes.cast(self._read_buf, ctypes.c_void_p),
            MAX_PAYLOAD_SIZE,
            ctypes.byref(self._out_len),
            ctypes.byref(self._out_ts),
        )
        if result != 0 or self._out_len.value == 0:
            return None
        data_len = self._out_len.value
        raw = bytes(self._read_buf[:data_len])
        return msgpack.unpackb(raw, raw=False)

    def read_with_timestamp(self, port_name):
        """Read latest data and timestamp. Returns (data, timestamp_ns) or (None, None)."""
        import ctypes

        result = self._lib.slpn_input_read(
            self._ctx_ptr,
            port_name.encode("utf-8"),
            ctypes.cast(self._read_buf, ctypes.c_void_p),
            MAX_PAYLOAD_SIZE,
            ctypes.byref(self._out_len),
            ctypes.byref(self._out_ts),
        )
        if result != 0 or self._out_len.value == 0:
            return None, None
        data_len = self._out_len.value
        raw = bytes(self._read_buf[:data_len])
        return msgpack.unpackb(raw, raw=False), self._out_ts.value


class NativeOutputs:
    """Output ports backed by iceoryx2 publishers via FFI."""

    def __init__(self, lib, ctx_ptr):
        self._lib = lib
        self._ctx_ptr = ctx_ptr

    def write(self, port_name, data, timestamp_ns=None):
        """Write data to a port. Serializes via msgpack and sends via FFI."""
        import ctypes

        if timestamp_ns is None:
            timestamp_ns = self._lib.slpn_context_time_ns(self._ctx_ptr)

        packed = msgpack.packb(data, use_bin_type=True)
        data_buf = (ctypes.c_uint8 * len(packed))(*packed)

        result = self._lib.slpn_output_write(
            self._ctx_ptr,
            port_name.encode("utf-8"),
            ctypes.cast(data_buf, ctypes.c_void_p),
            len(packed),
            timestamp_ns,
        )
        if result != 0:
            raise RuntimeError(f"Failed to write to port '{port_name}'")


class NativeGpuSurfaceHandle:
    """GPU surface handle backed by the C-side SurfaceHandle* via FFI.

    Delegates all operations to the cdylib. Does NOT use Python-side
    IOSurface ctypes — the cdylib owns the surface lifecycle.
    """

    def __init__(self, lib, handle_ptr):
        self._lib = lib
        self._handle_ptr = handle_ptr
        self.width = lib.slpn_gpu_surface_width(handle_ptr)
        self.height = lib.slpn_gpu_surface_height(handle_ptr)
        self.bytes_per_row = lib.slpn_gpu_surface_bytes_per_row(handle_ptr)

    def lock(self, read_only=True):
        """Lock surface for CPU access."""
        result = self._lib.slpn_gpu_surface_lock(
            self._handle_ptr, 1 if read_only else 0
        )
        if result != 0:
            raise RuntimeError("Failed to lock IOSurface")

    def unlock(self, read_only=True):
        """Unlock surface after CPU access."""
        self._lib.slpn_gpu_surface_unlock(
            self._handle_ptr, 1 if read_only else 0
        )

    def as_numpy(self):
        """Create numpy array VIEW into locked surface memory (zero-copy)."""
        import ctypes
        import numpy as np

        base = self._lib.slpn_gpu_surface_base_address(self._handle_ptr)
        if not base:
            raise RuntimeError("IOSurface base address is null (not locked?)")
        buf = (ctypes.c_uint8 * (self.bytes_per_row * self.height)).from_address(base)
        return np.ndarray(
            shape=(self.height, self.width, 4),
            dtype=np.uint8,
            buffer=buf,
            strides=(self.bytes_per_row, 4, 1),
        )

    @property
    def iosurface_id(self):
        """IOSurface ID for this surface."""
        return self._lib.slpn_gpu_surface_get_id(self._handle_ptr)

    @property
    def iosurface_ref(self):
        """Raw IOSurfaceRef pointer for CGL texture binding."""
        import ctypes
        ref = self._lib.slpn_gpu_surface_iosurface_ref(self._handle_ptr)
        return ctypes.c_void_p(ref)

    def release(self):
        """Release the C-side surface handle."""
        if self._handle_ptr:
            self._lib.slpn_gpu_surface_release(self._handle_ptr)
            self._handle_ptr = None

    def __del__(self):
        self.release()


class NativeGpu:
    """GPU context for IOSurface access via FFI, with broker XPC resolution."""

    def __init__(self, lib, broker_ptr):
        self._lib = lib
        self._broker_ptr = broker_ptr
        self._prev_acquired_pool_id = None

    def resolve_surface(self, surface_id):
        """Resolve a broker surface_id UUID to a GPU surface handle."""
        if self._broker_ptr:
            handle_ptr = self._lib.slpn_broker_resolve_surface(
                self._broker_ptr,
                surface_id.encode("utf-8"),
            )
            if not handle_ptr:
                raise RuntimeError(f"Broker failed to resolve surface: {surface_id}")
            return NativeGpuSurfaceHandle(self._lib, handle_ptr)

        # Fallback: treat surface_id as numeric IOSurface ID
        iosurface_id = int(surface_id)
        handle_ptr = self._lib.slpn_gpu_surface_lookup(iosurface_id)
        if not handle_ptr:
            raise RuntimeError(f"IOSurface not found: {surface_id}")
        return NativeGpuSurfaceHandle(self._lib, handle_ptr)

    def acquire_surface(self, width, height, format="bgra"):
        """Acquire a new pixel buffer via broker."""
        import ctypes

        bytes_per_element = 4  # BGRA

        # Unregister previous frame's surface from broker to prevent registry leak
        if self._broker_ptr and self._prev_acquired_pool_id is not None:
            self._lib.slpn_broker_unregister_surface(
                self._broker_ptr,
                self._prev_acquired_pool_id.encode("utf-8"),
            )

        if self._broker_ptr:
            pool_id_buf = (ctypes.c_char * 256)()
            handle_ptr = self._lib.slpn_broker_acquire_surface(
                self._broker_ptr,
                width, height, bytes_per_element,
                pool_id_buf, 256,
            )
            if not handle_ptr:
                raise RuntimeError(f"Broker failed to acquire surface: {width}x{height}")
            pool_id = pool_id_buf.value.decode("utf-8")
            self._prev_acquired_pool_id = pool_id
            return pool_id, NativeGpuSurfaceHandle(self._lib, handle_ptr)

        # Fallback: create IOSurface without broker
        handle_ptr = self._lib.slpn_gpu_surface_create(width, height, bytes_per_element)
        if not handle_ptr:
            raise RuntimeError(f"Failed to create IOSurface: {width}x{height}")
        surface_id = self._lib.slpn_gpu_surface_get_id(handle_ptr)
        return str(surface_id), NativeGpuSurfaceHandle(self._lib, handle_ptr)


class NativeProcessorContext:
    """Context using direct FFI to iceoryx2."""

    def __init__(self, lib, ctx_ptr, config, broker_ptr=None):
        self._lib = lib
        self._ctx_ptr = ctx_ptr
        self._config = config or {}
        self.inputs = NativeInputs(lib, ctx_ptr)
        self.outputs = NativeOutputs(lib, ctx_ptr)
        self.gpu = NativeGpu(lib, broker_ptr)

    @property
    def config(self):
        """Processor configuration dictionary."""
        return self._config

    @property
    def time(self):
        """Current monotonic time in nanoseconds."""
        return self._lib.slpn_context_time_ns(self._ctx_ptr)
