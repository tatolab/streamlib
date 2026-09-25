#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Core Audio for the audio fixtures on macOS, through ctypes and nothing else.

Two jobs, both engine-free and neither needing pyobjc:

- Name the Mac's built-in speaker and microphone by UID, so an acoustic run
  pins them and can never measure Camo, Wave Link or a Continuity iPhone.
- Turn this process's own output into a capture device: a private process tap
  of this process under a private aggregate device, which a `MicrophoneSource`
  in this same process opens by UID. It is the Mac's peer of a PipeWire null
  sink's monitor. It has to be made in the node, because the native built-ins
  run in the app process and a private device is visible only to the process
  that created it — which also means it dies with that process and, unlike the
  null sink, can never be stranded in the user's session.

A tap needs System Audio Recording for the app that launched this, and Core
Audio reports no error without it: the tap delivers exact zeros. The grant can
be preflighted through TCC without prompting, which is how a fixture tells a
missing grant from an engine that played nothing.

Only the device queries and the preflight are safe to run unattended. Creating
a tap raises the System Audio Recording prompt the first time it is read.
"""

import ctypes
import functools
import os
import struct
import sys
from typing import NamedTuple, Optional

CORE_AUDIO_FRAMEWORK = "/System/Library/Frameworks/CoreAudio.framework/CoreAudio"
CORE_FOUNDATION_FRAMEWORK = (
    "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
)
FOUNDATION_FRAMEWORK = "/System/Library/Frameworks/Foundation.framework/Foundation"
OBJECTIVE_C_RUNTIME_LIBRARY = "/usr/lib/libobjc.A.dylib"
TCC_PRIVATE_FRAMEWORK = "/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC"

# kTCCServiceAudioCapture: the service System Settings lists as "System Audio
# Recording Only". TCCAccessPreflight answers 0 for granted and 1 for denied.
SYSTEM_AUDIO_RECORDING_TCC_SERVICE = "kTCCServiceAudioCapture"
TCC_PREFLIGHT_ANSWERS = {0: "authorized", 1: "denied"}

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
TAP_PROPERTY_FORMAT = four_char_code("tfmt")

TRANSPORT_TYPE_BUILT_IN = "bltn"
# The data source a built-in device is routed through. Headphones on the jack
# are `hdpn`, so pinning the internal speaker also refuses headphones.
DATA_SOURCE_INTERNAL_SPEAKER = "ispk"
DATA_SOURCE_INTERNAL_MICROPHONE = "imic"

# CATapMuteBehavior. `muted` keeps the tapped process's audio off the hardware
# for as long as the tap exists, which is what makes the tap path silent.
TAP_MUTE_BEHAVIOURS = {"unmuted": 0, "muted": 1}

# The keys `AudioHardwareCreateAggregateDevice` reads, as AudioHardware.h spells
# them (kAudioAggregateDevice*Key, kAudioSubDevice*Key, kAudioSubTap*Key).
AGGREGATE_DEVICE_NAME_KEY = "name"
AGGREGATE_DEVICE_UID_KEY = "uid"
AGGREGATE_DEVICE_IS_PRIVATE_KEY = "private"
AGGREGATE_DEVICE_IS_STACKED_KEY = "stacked"
AGGREGATE_DEVICE_MAIN_SUB_DEVICE_KEY = "master"
AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY = "subdevices"
AGGREGATE_DEVICE_TAP_LIST_KEY = "taps"
SUB_DEVICE_UID_KEY = "uid"
SUB_TAP_UID_KEY = "uid"
SUB_TAP_DRIFT_COMPENSATION_KEY = "drift"


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


class _AudioObjectPropertyAddress(ctypes.Structure):
    _fields_ = [
        ("selector", ctypes.c_uint32),
        ("scope", ctypes.c_uint32),
        ("element", ctypes.c_uint32),
    ]


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


def _channel_count(object_id, scope):
    """Channels summed over a device's streams in one direction, from its AudioBufferList."""
    value = _property_bytes(object_id, DEVICE_PROPERTY_STREAM_CONFIGURATION, scope)
    if not value or len(value) < 4:
        return 0
    buffer_count = struct.unpack_from("<I", value)[0]
    # AudioBufferList: a UInt32 count, padded to 8, then 16-byte AudioBuffers
    # whose first field is mNumberChannels.
    return sum(
        struct.unpack_from("<I", value, 8 + 16 * index)[0]
        for index in range(buffer_count)
        if 8 + 16 * index + 4 <= len(value)
    )


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


def system_audio_recording_authorization() -> str:
    """TCC's answer for System Audio Recording, asked without prompting.

    Asked of the private TCC framework because Core Audio has no public query
    for it. `unknown` is what comes back when that framework will not answer.
    """
    try:
        tcc = ctypes.CDLL(TCC_PRIVATE_FRAMEWORK)
        preflight = tcc.TCCAccessPreflight
    except (OSError, AttributeError):
        return "unknown"
    preflight.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    preflight.restype = ctypes.c_int
    core_foundation = _mac_frameworks().core_foundation
    service = core_foundation.CFStringCreateWithCString(
        None, SYSTEM_AUDIO_RECORDING_TCC_SERVICE.encode("ascii"), CF_STRING_ENCODING_UTF8
    )
    try:
        return TCC_PREFLIGHT_ANSWERS.get(preflight(service, None), "not-determined")
    finally:
        core_foundation.CFRelease(service)


def aggregate_device_description(aggregate_device_uid, tap_uid, clock_device_uid):
    """The private aggregate a tap is read through, as `AudioHardwareCreateAggregateDevice` takes it.

    Clocked by the device the tapped audio plays to, so what the tap carries and
    what reads it share one clock and nothing drifts between them.
    """
    return {
        AGGREGATE_DEVICE_NAME_KEY: f"StreamLib fixture tap {aggregate_device_uid}",
        AGGREGATE_DEVICE_UID_KEY: aggregate_device_uid,
        AGGREGATE_DEVICE_IS_PRIVATE_KEY: 1,
        AGGREGATE_DEVICE_IS_STACKED_KEY: 0,
        AGGREGATE_DEVICE_MAIN_SUB_DEVICE_KEY: clock_device_uid,
        AGGREGATE_DEVICE_SUB_DEVICE_LIST_KEY: [{SUB_DEVICE_UID_KEY: clock_device_uid}],
        AGGREGATE_DEVICE_TAP_LIST_KEY: [
            {SUB_TAP_UID_KEY: tap_uid, SUB_TAP_DRIFT_COMPENSATION_KEY: 1}
        ],
    }


class _CoreFoundationObjectsToRelease:
    """Every CF object one call sequence created, released together when it ends."""

    def __init__(self) -> None:
        self._created: "list[int]" = []

    def __enter__(self) -> "_CoreFoundationObjectsToRelease":
        return self

    def __exit__(self, *_exception) -> None:
        core_foundation = _mac_frameworks().core_foundation
        for created in reversed(self._created):
            core_foundation.CFRelease(created)
        self._created.clear()

    def value_from(self, value) -> int:
        """A CFString, CFNumber, CFArray or CFDictionary holding a Python value.

        Toll-free bridged, so the same object serves where Foundation wants an
        NSString, NSNumber or NSArray.
        """
        frameworks = _mac_frameworks()
        core_foundation = frameworks.core_foundation
        if isinstance(value, str):
            created = core_foundation.CFStringCreateWithCString(
                None, value.encode("utf-8"), CF_STRING_ENCODING_UTF8
            )
        elif isinstance(value, int) and not isinstance(value, bool):
            number = ctypes.c_int32(value)
            created = core_foundation.CFNumberCreate(
                None, CF_NUMBER_SINT32_TYPE, ctypes.byref(number)
            )
        elif isinstance(value, list):
            items = (ctypes.c_void_p * len(value))(*[self.value_from(item) for item in value])
            created = core_foundation.CFArrayCreate(
                None,
                items,
                len(value),
                frameworks.core_foundation_callbacks_address("kCFTypeArrayCallBacks"),
            )
        elif isinstance(value, dict):
            keys = (ctypes.c_void_p * len(value))(*[self.value_from(key) for key in value])
            values = (ctypes.c_void_p * len(value))(
                *[self.value_from(item) for item in value.values()]
            )
            created = core_foundation.CFDictionaryCreate(
                None,
                keys,
                values,
                len(value),
                frameworks.core_foundation_callbacks_address("kCFTypeDictionaryKeyCallBacks"),
                frameworks.core_foundation_callbacks_address(
                    "kCFTypeDictionaryValueCallBacks"
                ),
            )
        else:
            raise TypeError(f"no Core Foundation type holds {type(value).__name__}")
        if not created:
            raise CoreAudioFixtureError(f"Core Foundation could not hold {value!r}")
        self._created.append(created)
        return created

    def description_of(self, cf_object) -> str:
        """What Core Foundation prints for an object, for a report or a test."""
        described = _mac_frameworks().core_foundation.CFCopyDescription(cf_object)
        self._created.append(described)
        return _python_string_from_cf_string(described) or ""


class _PrivateProcessTapDescription:
    """A `CATapDescription` of one process, alive for one `with` block.

    Only an Objective-C object: nothing is created in the HAL until
    `AudioHardwareCreateProcessTap` is handed it.
    """

    def __init__(self, process_object_id: int, mute_behaviour: str, name: str) -> None:
        self._process_object_id = process_object_id
        self._mute_behaviour = TAP_MUTE_BEHAVIOURS[mute_behaviour]
        self._name = name
        self._description = None
        self._autorelease_pool = None
        self._core_foundation_objects = _CoreFoundationObjectsToRelease()

    def __enter__(self) -> "_PrivateProcessTapDescription":
        frameworks = _mac_frameworks()
        tap_description_class = frameworks.objective_c_class("CATapDescription")
        if not tap_description_class:
            raise CoreAudioFixtureError(
                "this macOS has no CATapDescription; process taps need macOS 14.2"
            )
        self._autorelease_pool = frameworks.objective_c_runtime.objc_autoreleasePoolPush()
        try:
            processes = self._core_foundation_objects.value_from([self._process_object_id])
            allocated = frameworks.send_message(tap_description_class, "alloc")
            self._description = frameworks.send_message(
                allocated,
                "initStereoMixdownOfProcesses:",
                ctypes.c_void_p,
                (ctypes.c_void_p,),
                processes,
            )
            if not self._description:
                raise CoreAudioFixtureError("CATapDescription refused the process list")
            frameworks.send_message(
                self._description, "setPrivate:", None, (ctypes.c_bool,), True
            )
            frameworks.send_message(
                self._description,
                "setMuteBehavior:",
                None,
                (ctypes.c_long,),
                self._mute_behaviour,
            )
            frameworks.send_message(
                self._description,
                "setName:",
                None,
                (ctypes.c_void_p,),
                self._core_foundation_objects.value_from(self._name),
            )
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *_exception) -> None:
        frameworks = _mac_frameworks()
        if self._description:
            frameworks.send_message(self._description, "release", None)
            self._description = None
        self._core_foundation_objects.__exit__()
        if self._autorelease_pool:
            frameworks.objective_c_runtime.objc_autoreleasePoolPop(self._autorelease_pool)
            self._autorelease_pool = None

    @property
    def objective_c_object(self) -> int:
        return self._description

    def tap_uid(self) -> str:
        frameworks = _mac_frameworks()
        uuid = frameworks.send_message(self._description, "UUID")
        uuid_string = frameworks.send_message(uuid, "UUIDString")
        return frameworks.send_message(uuid_string, "UTF8String", ctypes.c_char_p).decode(
            "ascii"
        )

    def is_private(self) -> bool:
        return bool(
            _mac_frameworks().send_message(self._description, "isPrivate", ctypes.c_bool)
        )

    def mute_behaviour(self) -> str:
        value = _mac_frameworks().send_message(self._description, "isMuted", ctypes.c_long)
        return {number: name for name, number in TAP_MUTE_BEHAVIOURS.items()}.get(
            value, str(value)
        )


def _raise_on_failure(call_name: str, status: int) -> None:
    if status != 0:
        raise CoreAudioFixtureError(
            f"{call_name} failed with OSStatus {status} ('{four_char_code_text(status)}')"
        )


class PrivateCaptureDeviceTappingThisProcessesOutput:
    """This process's own output, readable as a capture device only it can see.

    Enter it before the graph opens a device, so a muted tap is muting from the
    first sample played rather than after an audible start. Leaving destroys
    the aggregate and then the tap; a process that dies first takes both with
    it, because both are private.
    """

    def __init__(
        self, aggregate_device_uid: str, mute_behaviour: str, clock_device_uid: str
    ) -> None:
        if mute_behaviour not in TAP_MUTE_BEHAVIOURS:
            raise ValueError(
                f"mute behaviour {mute_behaviour!r} is not one of "
                f"{', '.join(TAP_MUTE_BEHAVIOURS)}"
            )
        self.aggregate_device_uid = aggregate_device_uid
        self.mute_behaviour = mute_behaviour
        self.clock_device_uid = clock_device_uid
        self.tap_uid: Optional[str] = None
        self.tap_object_id = AUDIO_OBJECT_UNKNOWN
        self.aggregate_device_object_id = AUDIO_OBJECT_UNKNOWN

    def __enter__(self) -> "PrivateCaptureDeviceTappingThisProcessesOutput":
        core_audio = _mac_frameworks().core_audio
        this_process_object = process_object_of(os.getpid())
        if this_process_object == AUDIO_OBJECT_UNKNOWN:
            raise CoreAudioFixtureError(
                f"the HAL has no process object for this process ({os.getpid()}), "
                "so there is nothing to tap"
            )
        with _PrivateProcessTapDescription(
            this_process_object, self.mute_behaviour, f"StreamLib fixture tap of {os.getpid()}"
        ) as tap_description:
            self.tap_uid = tap_description.tap_uid()
            tap_object_id = ctypes.c_uint32(AUDIO_OBJECT_UNKNOWN)
            _raise_on_failure(
                "AudioHardwareCreateProcessTap",
                core_audio.AudioHardwareCreateProcessTap(
                    tap_description.objective_c_object, ctypes.byref(tap_object_id)
                ),
            )
            self.tap_object_id = tap_object_id.value
        try:
            with _CoreFoundationObjectsToRelease() as core_foundation_objects:
                description = core_foundation_objects.value_from(
                    aggregate_device_description(
                        self.aggregate_device_uid, self.tap_uid, self.clock_device_uid
                    )
                )
                aggregate_device_object_id = ctypes.c_uint32(AUDIO_OBJECT_UNKNOWN)
                _raise_on_failure(
                    "AudioHardwareCreateAggregateDevice",
                    core_audio.AudioHardwareCreateAggregateDevice(
                        description, ctypes.byref(aggregate_device_object_id)
                    ),
                )
                self.aggregate_device_object_id = aggregate_device_object_id.value
        except BaseException:
            self.__exit__(None, None, None)
            raise
        return self

    def __exit__(self, *_exception) -> None:
        core_audio = _mac_frameworks().core_audio
        if self.aggregate_device_object_id != AUDIO_OBJECT_UNKNOWN:
            core_audio.AudioHardwareDestroyAggregateDevice(self.aggregate_device_object_id)
            self.aggregate_device_object_id = AUDIO_OBJECT_UNKNOWN
        if self.tap_object_id != AUDIO_OBJECT_UNKNOWN:
            core_audio.AudioHardwareDestroyProcessTap(self.tap_object_id)
            self.tap_object_id = AUDIO_OBJECT_UNKNOWN

    def tap_format(self) -> "tuple[float, int] | None":
        """The tap's sample rate and channel count, from its AudioStreamBasicDescription."""
        value = _property_bytes(self.tap_object_id, TAP_PROPERTY_FORMAT)
        if not value or len(value) < 32:
            return None
        sample_rate = struct.unpack_from("<d", value, 0)[0]
        channels = struct.unpack_from("<I", value, 28)[0]
        return sample_rate, channels

    def evidence(self) -> str:
        """One line naming what was created, for the node's log."""
        tap_format = self.tap_format()
        tap_format_text = (
            f"{tap_format[0]:.0f}Hz/{tap_format[1]}ch" if tap_format else "unreadable"
        )
        aggregate = describe_device(self.aggregate_device_object_id)
        return (
            f"aggregate_device_uid={self.aggregate_device_uid} tap_uid={self.tap_uid} "
            f"mute_behaviour={self.mute_behaviour} clock_device_uid={self.clock_device_uid} "
            f"tap_format={tap_format_text} "
            f"aggregate_format={aggregate.nominal_sample_rate:.0f}Hz/"
            f"{aggregate.input_channels}ch-in"
        )


def _print_the_uid_of(device: Optional[CoreAudioDevice], what: str) -> int:
    if device is None:
        print(f"no {what} on this Mac", file=sys.stderr)
        return 1
    print(device.uid)
    return 0


def main(argv) -> int:
    command = argv[1] if len(argv) > 1 else ""
    if command == "devices":
        for device in attached_devices():
            print(
                f"{device.uid!r} {device.name!r} transport={device.transport_type} "
                f"in={device.input_channels}/{device.input_data_source or '-'} "
                f"out={device.output_channels}/{device.output_data_source or '-'} "
                f"rate={device.nominal_sample_rate:.0f}"
            )
        return 0
    if command == "built-in-speaker-uid":
        return _print_the_uid_of(built_in_speaker_among(attached_devices()), "built-in speaker")
    if command == "built-in-microphone-uid":
        return _print_the_uid_of(
            built_in_microphone_among(attached_devices()), "built-in microphone"
        )
    if command == "built-in-microphone-name":
        microphone = built_in_microphone_among(attached_devices())
        if microphone is None:
            print("no built-in microphone on this Mac", file=sys.stderr)
            return 1
        print(microphone.name)
        return 0
    if command == "default-output-uid":
        return _print_the_uid_of(default_output_device(), "default output device")
    if command == "system-audio-recording-authorization":
        print(system_audio_recording_authorization())
        return 0
    print(
        "Usage: coreaudio_process_tap.py devices | built-in-speaker-uid | "
        "built-in-microphone-uid | built-in-microphone-name | default-output-uid | "
        "system-audio-recording-authorization",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
