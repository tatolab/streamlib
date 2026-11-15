# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Core Audio HAL as the audio fixtures read it on macOS, through ctypes alone.

The frameworks loaded once, the property selectors and the structures the HAL
hands back, each device's streams, formats and data source, and the Mac's
built-in speaker and microphone named by UID. `coreaudio_process_tap` builds its
taps and aggregate devices on this.
"""

import ctypes
import functools
import struct
import sys
from typing import NamedTuple, Optional

CORE_AUDIO_FRAMEWORK = "/System/Library/Frameworks/CoreAudio.framework/CoreAudio"
CORE_FOUNDATION_FRAMEWORK = (
    "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
)
FOUNDATION_FRAMEWORK = "/System/Library/Frameworks/Foundation.framework/Foundation"
OBJECTIVE_C_RUNTIME_LIBRARY = "/usr/lib/libobjc.A.dylib"

CF_STRING_ENCODING_UTF8 = 0x08000100
CF_NUMBER_SINT32_TYPE = 3


def four_char_code(code: str) -> int:
    """A Core Audio selector or constant, from the four characters the headers spell."""
    return struct.unpack(">I", code.encode("ascii"))[0]


def four_char_code_text(value: int) -> str:
    """The four characters a Core Audio code spells, or its number when it spells none."""
    spelled = struct.pack(">I", value & 0xFFFFFFFF)
    if all(32 <= byte < 127 for byte in spelled):
        return spelled.decode("ascii")
    return str(value)


AUDIO_SYSTEM_OBJECT_ID = 1
AUDIO_OBJECT_UNKNOWN = 0
PROPERTY_ELEMENT_MAIN = 0
PROPERTY_SCOPE_GLOBAL = four_char_code("glob")
PROPERTY_SCOPE_INPUT = four_char_code("inpt")
PROPERTY_SCOPE_OUTPUT = four_char_code("outp")

HARDWARE_PROPERTY_DEVICES = four_char_code("dev#")
HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE = four_char_code("dOut")
HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT = four_char_code("id2p")
DEVICE_PROPERTY_DEVICE_UID = four_char_code("uid ")
OBJECT_PROPERTY_NAME = four_char_code("lnam")
DEVICE_PROPERTY_TRANSPORT_TYPE = four_char_code("tran")
DEVICE_PROPERTY_STREAM_CONFIGURATION = four_char_code("slay")
DEVICE_PROPERTY_DATA_SOURCE = four_char_code("ssrc")
DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE = four_char_code("nsrt")
DEVICE_PROPERTY_STREAMS = four_char_code("stm#")
STREAM_PROPERTY_VIRTUAL_FORMAT = four_char_code("sfmt")
TAP_PROPERTY_FORMAT = four_char_code("tfmt")

AUDIO_FORMAT_LINEAR_PCM = "lpcm"
AUDIO_FORMAT_FLAG_IS_FLOAT = 1 << 0
AUDIO_TIME_STAMP_SAMPLE_TIME_VALID = 1 << 0

TRANSPORT_TYPE_BUILT_IN = "bltn"
# The data source a built-in device is routed through. Headphones on the jack
# are `hdpn`, so pinning the internal speaker also refuses headphones.
DATA_SOURCE_INTERNAL_SPEAKER = "ispk"
DATA_SOURCE_INTERNAL_MICROPHONE = "imic"



class CoreAudioFixtureError(RuntimeError):
    """A Core Audio call the fixture depends on refused, named with its status."""


class CoreAudioDevice(NamedTuple):
    """One device the HAL lists, with what the fixtures tell devices apart by."""

    object_id: int
    uid: str
    name: str
    transport_type: str
    input_channels: int
    output_channels: int
    input_data_source: str
    output_data_source: str
    nominal_sample_rate: float


class AudioStreamFormat(NamedTuple):
    """The fields of one stream's AudioStreamBasicDescription a fixture checks."""

    sample_rate: float
    format_id: str
    format_flags: int
    bits_per_channel: int
    channels_per_frame: int

    @property
    def is_32_bit_float(self) -> bool:
        return (
            self.format_id == AUDIO_FORMAT_LINEAR_PCM
            and bool(self.format_flags & AUDIO_FORMAT_FLAG_IS_FLOAT)
            and self.bits_per_channel == 32
        )


class _AudioObjectPropertyAddress(ctypes.Structure):
    _fields_ = [
        ("selector", ctypes.c_uint32),
        ("scope", ctypes.c_uint32),
        ("element", ctypes.c_uint32),
    ]


class AudioBuffer(ctypes.Structure):
    """CoreAudioTypes.h's AudioBuffer: 16 bytes on a 64-bit Mac."""

    _fields_ = [
        ("number_of_channels", ctypes.c_uint32),
        ("data_byte_size", ctypes.c_uint32),
        ("data", ctypes.c_void_p),
    ]


# AudioBufferList is a UInt32 count, padded to the 8-byte alignment of the
# AudioBuffers that follow it.
AUDIO_BUFFER_LIST_FIRST_BUFFER_OFFSET = 8
# AudioTimeStamp: mSampleTime is the Float64 at 0, mFlags the UInt32 at 56.
AUDIO_TIME_STAMP_FLAGS_OFFSET = 56

# AudioHardware.h's AudioDeviceIOProc: the device, now, the input buffers and
# their time, the output buffers and their time, and the client data.
AUDIO_DEVICE_IO_PROC = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.c_uint32,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.c_void_p,
)


def audio_buffers_at(audio_buffer_list_address) -> "ctypes.Array[AudioBuffer]":
    """The AudioBuffers an AudioBufferList holds, read in place; none for a null list."""
    if not audio_buffer_list_address:
        return (AudioBuffer * 0)()
    count = ctypes.c_uint32.from_address(audio_buffer_list_address).value
    return (AudioBuffer * count).from_address(
        audio_buffer_list_address + AUDIO_BUFFER_LIST_FIRST_BUFFER_OFFSET
    )


def sample_time_at(audio_time_stamp_address) -> Optional[float]:
    """An AudioTimeStamp's sample time, or None when it carries none."""
    if not audio_time_stamp_address:
        return None
    flags = ctypes.c_uint32.from_address(
        audio_time_stamp_address + AUDIO_TIME_STAMP_FLAGS_OFFSET
    ).value
    if not flags & AUDIO_TIME_STAMP_SAMPLE_TIME_VALID:
        return None
    return ctypes.c_double.from_address(audio_time_stamp_address).value


class _MacFrameworks:
    """CoreAudio, CoreFoundation and the Objective-C runtime with their prototypes."""

    def __init__(self) -> None:
        self.core_audio = ctypes.CDLL(CORE_AUDIO_FRAMEWORK)
        self.core_foundation = ctypes.CDLL(CORE_FOUNDATION_FRAMEWORK)
        # Loaded for its classes: CATapDescription is an NSObject that takes
        # NSArray and NSNumber arguments.
        ctypes.CDLL(FOUNDATION_FRAMEWORK)
        self.objective_c_runtime = ctypes.CDLL(OBJECTIVE_C_RUNTIME_LIBRARY)
        self._declare_core_audio_prototypes()
        self._declare_core_foundation_prototypes()
        self._declare_objective_c_runtime_prototypes()

    def _declare_core_audio_prototypes(self) -> None:
        core_audio = self.core_audio
        address = ctypes.POINTER(_AudioObjectPropertyAddress)
        core_audio.AudioObjectGetPropertyDataSize.argtypes = [
            ctypes.c_uint32,
            address,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        core_audio.AudioObjectGetPropertyDataSize.restype = ctypes.c_int32
        core_audio.AudioObjectGetPropertyData.argtypes = [
            ctypes.c_uint32,
            address,
            ctypes.c_uint32,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
            ctypes.c_void_p,
        ]
        core_audio.AudioObjectGetPropertyData.restype = ctypes.c_int32
        core_audio.AudioHardwareCreateProcessTap.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        core_audio.AudioHardwareCreateProcessTap.restype = ctypes.c_int32
        core_audio.AudioHardwareDestroyProcessTap.argtypes = [ctypes.c_uint32]
        core_audio.AudioHardwareDestroyProcessTap.restype = ctypes.c_int32
        core_audio.AudioHardwareCreateAggregateDevice.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        core_audio.AudioHardwareCreateAggregateDevice.restype = ctypes.c_int32
        core_audio.AudioHardwareDestroyAggregateDevice.argtypes = [ctypes.c_uint32]
        core_audio.AudioHardwareDestroyAggregateDevice.restype = ctypes.c_int32
        core_audio.AudioDeviceCreateIOProcID.argtypes = [
            ctypes.c_uint32,
            AUDIO_DEVICE_IO_PROC,
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        core_audio.AudioDeviceCreateIOProcID.restype = ctypes.c_int32
        for call_taking_a_device_and_an_io_proc_id in (
            core_audio.AudioDeviceDestroyIOProcID,
            core_audio.AudioDeviceStart,
            core_audio.AudioDeviceStop,
        ):
            call_taking_a_device_and_an_io_proc_id.argtypes = [
                ctypes.c_uint32,
                ctypes.c_void_p,
            ]
            call_taking_a_device_and_an_io_proc_id.restype = ctypes.c_int32

    def _declare_core_foundation_prototypes(self) -> None:
        core_foundation = self.core_foundation
        core_foundation.CFStringCreateWithCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_uint32,
        ]
        core_foundation.CFStringCreateWithCString.restype = ctypes.c_void_p
        core_foundation.CFStringGetLength.argtypes = [ctypes.c_void_p]
        core_foundation.CFStringGetLength.restype = ctypes.c_long
        core_foundation.CFStringGetMaximumSizeForEncoding.argtypes = [
            ctypes.c_long,
            ctypes.c_uint32,
        ]
        core_foundation.CFStringGetMaximumSizeForEncoding.restype = ctypes.c_long
        core_foundation.CFStringGetCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_long,
            ctypes.c_uint32,
        ]
        core_foundation.CFStringGetCString.restype = ctypes.c_bool
        core_foundation.CFNumberCreate.argtypes = [
            ctypes.c_void_p,
            ctypes.c_long,
            ctypes.c_void_p,
        ]
        core_foundation.CFNumberCreate.restype = ctypes.c_void_p
        core_foundation.CFArrayCreate.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.c_long,
            ctypes.c_void_p,
        ]
        core_foundation.CFArrayCreate.restype = ctypes.c_void_p
        core_foundation.CFDictionaryCreate.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.c_long,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        core_foundation.CFDictionaryCreate.restype = ctypes.c_void_p
        core_foundation.CFCopyDescription.argtypes = [ctypes.c_void_p]
        core_foundation.CFCopyDescription.restype = ctypes.c_void_p
        core_foundation.CFRelease.argtypes = [ctypes.c_void_p]
        core_foundation.CFRelease.restype = None

    def _declare_objective_c_runtime_prototypes(self) -> None:
        runtime = self.objective_c_runtime
        runtime.objc_getClass.argtypes = [ctypes.c_char_p]
        runtime.objc_getClass.restype = ctypes.c_void_p
        runtime.sel_registerName.argtypes = [ctypes.c_char_p]
        runtime.sel_registerName.restype = ctypes.c_void_p
        runtime.objc_autoreleasePoolPush.argtypes = []
        runtime.objc_autoreleasePoolPush.restype = ctypes.c_void_p
        runtime.objc_autoreleasePoolPop.argtypes = [ctypes.c_void_p]
        runtime.objc_autoreleasePoolPop.restype = None
        self._objc_msg_send_address = ctypes.cast(
            runtime.objc_msgSend, ctypes.c_void_p
        ).value

    def core_foundation_callbacks_address(self, symbol: str) -> int:
        """Where one of CoreFoundation's exported callback tables lives."""
        return ctypes.addressof(ctypes.c_char.in_dll(self.core_foundation, symbol))

    def objective_c_class(self, class_name: str) -> Optional[int]:
        return self.objective_c_runtime.objc_getClass(class_name.encode("ascii"))

    def send_message(
        self, receiver, selector_name, result_type=ctypes.c_void_p, argument_types=(), *arguments
    ):
        """`objc_msgSend` cast to this message's exact prototype.

        Cast per call rather than declared variadic: arm64 passes variadic
        arguments on the stack, where the method reads them from registers.
        """
        prototype = ctypes.CFUNCTYPE(
            result_type, ctypes.c_void_p, ctypes.c_void_p, *argument_types
        )
        selector = self.objective_c_runtime.sel_registerName(
            selector_name.encode("ascii")
        )
        return prototype(self._objc_msg_send_address)(receiver, selector, *arguments)


@functools.lru_cache(maxsize=1)
def _mac_frameworks() -> _MacFrameworks:
    if sys.platform != "darwin":
        raise CoreAudioFixtureError(f"Core Audio is macOS's; this is {sys.platform}")
    return _MacFrameworks()


def _python_string_from_cf_string(cf_string) -> Optional[str]:
    core_foundation = _mac_frameworks().core_foundation
    length = core_foundation.CFStringGetLength(cf_string)
    capacity = (
        core_foundation.CFStringGetMaximumSizeForEncoding(length, CF_STRING_ENCODING_UTF8)
        + 1
    )
    buffer = ctypes.create_string_buffer(capacity)
    if not core_foundation.CFStringGetCString(
        cf_string, buffer, capacity, CF_STRING_ENCODING_UTF8
    ):
        return None
    return buffer.value.decode("utf-8")


def _property_bytes(object_id, selector, scope=PROPERTY_SCOPE_GLOBAL, qualifier=None):
    """A property's raw value, or None where the object does not have it."""
    core_audio = _mac_frameworks().core_audio
    address = _AudioObjectPropertyAddress(selector, scope, PROPERTY_ELEMENT_MAIN)
    qualifier_size = ctypes.sizeof(qualifier) if qualifier is not None else 0
    qualifier_pointer = ctypes.byref(qualifier) if qualifier is not None else None
    size = ctypes.c_uint32(0)
    status = core_audio.AudioObjectGetPropertyDataSize(
        object_id, ctypes.byref(address), qualifier_size, qualifier_pointer, ctypes.byref(size)
    )
    if status != 0 or size.value == 0:
        return None
    value = ctypes.create_string_buffer(size.value)
    status = core_audio.AudioObjectGetPropertyData(
        object_id,
        ctypes.byref(address),
        qualifier_size,
        qualifier_pointer,
        ctypes.byref(size),
        value,
    )
    if status != 0:
        return None
    return value.raw[: size.value]


def _uint32_property(object_id, selector, scope=PROPERTY_SCOPE_GLOBAL, qualifier=None):
    value = _property_bytes(object_id, selector, scope, qualifier)
    return struct.unpack_from("<I", value)[0] if value and len(value) >= 4 else None


def _float64_property(object_id, selector, scope=PROPERTY_SCOPE_GLOBAL):
    value = _property_bytes(object_id, selector, scope)
    return struct.unpack_from("<d", value)[0] if value and len(value) >= 8 else None


def _string_property(object_id, selector, scope=PROPERTY_SCOPE_GLOBAL):
    """A CFString property as Python text; the HAL hands the caller a +1 reference."""
    value = _property_bytes(object_id, selector, scope)
    if not value or len(value) < ctypes.sizeof(ctypes.c_void_p):
        return None
    cf_string = ctypes.c_void_p.from_buffer_copy(value).value
    if not cf_string:
        return None
    try:
        return _python_string_from_cf_string(cf_string)
    finally:
        _mac_frameworks().core_foundation.CFRelease(cf_string)


def channels_in_each_buffer(object_id, scope) -> "list[int]":
    """The channel count of each buffer an IOProc on this device sees in one direction."""
    value = _property_bytes(object_id, DEVICE_PROPERTY_STREAM_CONFIGURATION, scope)
    if not value or len(value) < 4:
        return []
    buffer_count = struct.unpack_from("<I", value)[0]
    buffer_size = ctypes.sizeof(AudioBuffer)
    return [
        struct.unpack_from(
            "<I", value, AUDIO_BUFFER_LIST_FIRST_BUFFER_OFFSET + buffer_size * index
        )[0]
        for index in range(buffer_count)
        if AUDIO_BUFFER_LIST_FIRST_BUFFER_OFFSET + buffer_size * index + 4 <= len(value)
    ]


def _channel_count(object_id, scope):
    return sum(channels_in_each_buffer(object_id, scope))


def stream_virtual_formats(object_id, scope) -> "list[AudioStreamFormat]":
    """The format an IOProc sees on each of a device's streams in one direction."""
    value = _property_bytes(object_id, DEVICE_PROPERTY_STREAMS, scope) or b""
    stream_ids = struct.unpack(f"<{len(value) // 4}I", value[: len(value) // 4 * 4])
    formats = []
    for stream_id in stream_ids:
        # AudioStreamBasicDescription: mSampleRate, mFormatID and mFormatFlags
        # lead; mChannelsPerFrame and mBitsPerChannel sit at 28 and 32.
        description = _property_bytes(stream_id, STREAM_PROPERTY_VIRTUAL_FORMAT)
        if not description or len(description) < 36:
            continue
        sample_rate, format_id, format_flags = struct.unpack_from("<dII", description, 0)
        channels_per_frame, bits_per_channel = struct.unpack_from("<II", description, 28)
        formats.append(
            AudioStreamFormat(
                sample_rate=sample_rate,
                format_id=four_char_code_text(format_id),
                format_flags=format_flags,
                bits_per_channel=bits_per_channel,
                channels_per_frame=channels_per_frame,
            )
        )
    return formats


def _data_source(object_id, scope):
    value = _uint32_property(object_id, DEVICE_PROPERTY_DATA_SOURCE, scope)
    return four_char_code_text(value) if value is not None else ""


def describe_device(object_id: int) -> CoreAudioDevice:
    transport = _uint32_property(object_id, DEVICE_PROPERTY_TRANSPORT_TYPE)
    return CoreAudioDevice(
        object_id=object_id,
        uid=_string_property(object_id, DEVICE_PROPERTY_DEVICE_UID) or "",
        name=_string_property(object_id, OBJECT_PROPERTY_NAME) or "",
        transport_type=four_char_code_text(transport) if transport is not None else "",
        input_channels=_channel_count(object_id, PROPERTY_SCOPE_INPUT),
        output_channels=_channel_count(object_id, PROPERTY_SCOPE_OUTPUT),
        input_data_source=_data_source(object_id, PROPERTY_SCOPE_INPUT),
        output_data_source=_data_source(object_id, PROPERTY_SCOPE_OUTPUT),
        nominal_sample_rate=_float64_property(object_id, DEVICE_PROPERTY_NOMINAL_SAMPLE_RATE)
        or 0.0,
    )


def attached_devices() -> "list[CoreAudioDevice]":
    """Every device the HAL lists to this process, private ones it created included."""
    value = _property_bytes(AUDIO_SYSTEM_OBJECT_ID, HARDWARE_PROPERTY_DEVICES) or b""
    object_ids = struct.unpack(f"<{len(value) // 4}I", value[: len(value) // 4 * 4])
    return [describe_device(object_id) for object_id in object_ids]


def device_with_uid(uid: str) -> Optional[CoreAudioDevice]:
    for device in attached_devices():
        if device.uid == uid:
            return device
    return None


def default_output_device() -> Optional[CoreAudioDevice]:
    object_id = _uint32_property(AUDIO_SYSTEM_OBJECT_ID, HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE)
    return describe_device(object_id) if object_id else None


def built_in_speaker_among(devices) -> Optional[CoreAudioDevice]:
    """The internal speaker, never headphones on the jack or anything virtual."""
    for device in devices:
        if (
            device.transport_type == TRANSPORT_TYPE_BUILT_IN
            and device.output_channels > 0
            and device.output_data_source == DATA_SOURCE_INTERNAL_SPEAKER
        ):
            return device
    return None


def built_in_microphone_among(devices) -> Optional[CoreAudioDevice]:
    """The internal microphone, never a headset microphone or anything virtual."""
    for device in devices:
        if (
            device.transport_type == TRANSPORT_TYPE_BUILT_IN
            and device.input_channels > 0
            and device.input_data_source == DATA_SOURCE_INTERNAL_MICROPHONE
        ):
            return device
    return None


def process_object_of(process_id: int) -> int:
    """The HAL's object for a process, or 0 when that process is not its client."""
    return (
        _uint32_property(
            AUDIO_SYSTEM_OBJECT_ID,
            HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT,
            qualifier=ctypes.c_int32(process_id),
        )
        or AUDIO_OBJECT_UNKNOWN
    )
