#!/usr/bin/env python3
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Core Audio for the audio fixtures on macOS, through ctypes and nothing else.

Engine-free, and none of it needs pyobjc:

- Name the Mac's built-in speaker and microphone by UID, so an acoustic run
  pins them and can never measure Camo, Wave Link or a Continuity iPhone.
- Turn this process's own output into a capture device: a private process tap
  of this process under a private aggregate device, opened by UID in this same
  process. It is the Mac's peer of a PipeWire null sink's monitor. A private
  device is visible only to the process that created it, so it dies with that
  process and, unlike the null sink, can never be stranded in the user's
  session.
- Turn every process's output into a capture device another process can open:
  a shared global tap under a shared aggregate device, held by this script
  until it is signalled. This is how a fixture hands the native built-ins in
  `tatolabd` a loopback, since a private device made outside `tatolabd` is
  invisible to it.
- Run an IOProc on a device, which is how the rig peer plays and records
  through that same tap with no StreamLib in the path.

A tap needs System Audio Recording for the app that launched this, and Core
Audio reports no error without it: the tap delivers exact zeros. The grant can
be preflighted through TCC without prompting, which is how a fixture tells a
missing grant from an engine that played nothing — and how it decides, before
anything is made, whether a tap may be made at all.

Only the device queries and the preflight are safe to run unattended. Creating
a tap raises the System Audio Recording prompt the first time it is read.

The HAL bindings and the device queries live in `coreaudio_hal_devices`.
"""

import ctypes
import os
import signal
import struct
import sys
from typing import Optional

from coreaudio_hal_devices import (
    attached_devices,
    AUDIO_DEVICE_IO_PROC,
    AUDIO_OBJECT_UNKNOWN,
    built_in_microphone_among,
    built_in_speaker_among,
    CF_STRING_ENCODING_UTF8,
    CoreAudioDevice,
    CoreAudioFixtureError,
    default_output_device,
    describe_device,
    four_char_code,
    four_char_code_text,
    process_object_of,
    mac_frameworks,
    property_bytes,
    python_string_from_cf_string,
)

TCC_PRIVATE_FRAMEWORK = "/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC"

# kTCCServiceAudioCapture: the service System Settings lists as "System Audio
# Recording Only". TCCAccessPreflight answers 0 for granted and 1 for denied.
SYSTEM_AUDIO_RECORDING_TCC_SERVICE = "kTCCServiceAudioCapture"
TCC_PREFLIGHT_ANSWERS = {0: "authorized", 1: "denied"}

# Set to 1 by whoever is at the machine and listening; nothing else sets it.
ATTENDED_RUN_ENVIRONMENT_VARIABLE = "STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS"

GRANT_SYSTEM_AUDIO_RECORDING_IN_SYSTEM_SETTINGS = (
    "Grant it in System Settings › Privacy & Security › Screen & System Audio "
    "Recording › System Audio Recording Only (add Terminal, or whichever app "
    "launched this), then run again."
)

# CATapMuteBehavior. `muted` keeps the tapped process's audio off the hardware
# for as long as the tap exists, which is what makes the tap path silent.
TAP_MUTE_BEHAVIOURS = {"unmuted": 0, "muted": 1}

CF_NUMBER_SINT32_TYPE = 3
TAP_PROPERTY_FORMAT = four_char_code("tfmt")

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
    core_foundation = mac_frameworks().core_foundation
    service = core_foundation.CFStringCreateWithCString(
        None, SYSTEM_AUDIO_RECORDING_TCC_SERVICE.encode("ascii"), CF_STRING_ENCODING_UTF8
    )
    try:
        return TCC_PREFLIGHT_ANSWERS.get(preflight(service, None), "not-determined")
    finally:
        core_foundation.CFRelease(service)


def this_run_is_attended() -> bool:
    return os.environ.get(ATTENDED_RUN_ENVIRONMENT_VARIABLE) == "1"


def refusal_to_create_a_tap(authorization: str, attended: bool) -> Optional[str]:
    """Why no tap may be made now, or None when one may.

    Fails closed. Making a tap without a grant raises the prompt, and whether
    a muted tap mutes before the prompt is answered is unknown, so unattended
    only a grant TCC has confirmed lets one be made. A denial refuses even
    attended: that tap could deliver nothing but zeros.
    """
    if authorization == "authorized":
        return None
    if authorization == "denied":
        return (
            "TCC says System Audio Recording is denied for the app that launched "
            "this, so a process tap could deliver nothing but zeros. "
            + GRANT_SYSTEM_AUDIO_RECORDING_IN_SYSTEM_SETTINGS
        )
    if attended:
        return None
    if authorization == "not-determined":
        why = "System Audio Recording has never been answered for the app that launched this"
    elif authorization == "unknown":
        why = "TCC would not say whether System Audio Recording is granted"
    else:
        why = f"TCC answered {authorization!r} about System Audio Recording"
    return (
        f"{why}, and creating a tap would ask — a prompt, and possibly sound until "
        f"it is answered. Run this once attended, with {ATTENDED_RUN_ENVIRONMENT_VARIABLE}=1, "
        "and click Allow. Otherwise: "
        + GRANT_SYSTEM_AUDIO_RECORDING_IN_SYSTEM_SETTINGS
    )


def why_a_tap_delivered_exact_zeros(
    authorization_before_the_run: str, authorization_now: str
) -> "tuple[int, str]":
    """The exit status and reason for a tap capture in which every sample is zero.

    1 only when the grant held from before the run to after it, because only
    then are the zeros what was played. Otherwise a missing grant explains
    them, or TCC cannot say whether one does, and the run is 77.
    """
    if authorization_now == "authorized" and authorization_before_the_run == "authorized":
        return 1, (
            "ERROR: the tap heard exact zeros with System Audio Recording authorized "
            "throughout — nothing this process played reached the tap"
        )
    if authorization_now == "authorized":
        return 77, (
            "SKIP: the process tap delivered exact zeros, and System Audio Recording was "
            f"granted during this run ({authorization_before_the_run} when it started) — "
            "run again now that it is authorized."
        )
    if authorization_now == "unknown":
        return 77, (
            "SKIP: the process tap delivered exact zeros, and TCC would not say whether "
            "System Audio Recording is granted, so a missing grant cannot be told from "
            "a player that played nothing."
        )
    return 77, (
        "SKIP: the process tap delivered exact zeros, and TCC says System Audio "
        f"Recording is {authorization_now} for the app that launched this. "
        + GRANT_SYSTEM_AUDIO_RECORDING_IN_SYSTEM_SETTINGS
    )


def aggregate_device_description(
    aggregate_device_uid, tap_uid, clock_device_uid, is_private=True
):
    """The aggregate a tap is read through, as `AudioHardwareCreateAggregateDevice` takes it.

    Clocked by the device the tapped audio plays to, so what the tap carries and
    what reads it share one clock and nothing drifts between them.
    """
    return {
        AGGREGATE_DEVICE_NAME_KEY: f"StreamLib fixture tap {aggregate_device_uid}",
        AGGREGATE_DEVICE_UID_KEY: aggregate_device_uid,
        AGGREGATE_DEVICE_IS_PRIVATE_KEY: 1 if is_private else 0,
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
        core_foundation = mac_frameworks().core_foundation
        for created in reversed(self._created):
            core_foundation.CFRelease(created)
        self._created.clear()

    def value_from(self, value) -> int:
        """A CFString, CFNumber, CFArray or CFDictionary holding a Python value.

        Toll-free bridged, so the same object serves where Foundation wants an
        NSString, NSNumber or NSArray.
        """
        frameworks = mac_frameworks()
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
        described = mac_frameworks().core_foundation.CFCopyDescription(cf_object)
        self._created.append(described)
        return python_string_from_cf_string(described) or ""


class _TapDescription:
    """A `CATapDescription`, alive for one `with` block.

    Only an Objective-C object: nothing is created in the HAL until
    `AudioHardwareCreateProcessTap` is handed it. `initialiser_selector` takes
    one NSArray of process objects: the processes tapped, or the processes a
    global tap leaves out.
    """

    def __init__(
        self,
        initialiser_selector: str,
        process_object_ids: "list[int]",
        is_private: bool,
        mute_behaviour: str,
        name: str,
    ) -> None:
        self._initialiser_selector = initialiser_selector
        self._process_object_ids = process_object_ids
        self._is_private = is_private
        self._mute_behaviour = TAP_MUTE_BEHAVIOURS[mute_behaviour]
        self._name = name
        self._description = None
        self._autorelease_pool = None
        self._core_foundation_objects = _CoreFoundationObjectsToRelease()

    def __enter__(self) -> "_TapDescription":
        frameworks = mac_frameworks()
        tap_description_class = frameworks.objective_c_class("CATapDescription")
        if not tap_description_class:
            raise CoreAudioFixtureError(
                "this macOS has no CATapDescription; process taps need macOS 14.2"
            )
        self._autorelease_pool = frameworks.objective_c_runtime.objc_autoreleasePoolPush()
        try:
            processes = self._core_foundation_objects.value_from(self._process_object_ids)
            allocated = frameworks.send_message(tap_description_class, "alloc")
            self._description = frameworks.send_message(
                allocated,
                self._initialiser_selector,
                ctypes.c_void_p,
                (ctypes.c_void_p,),
                processes,
            )
            if not self._description:
                raise CoreAudioFixtureError("CATapDescription refused the process list")
            frameworks.send_message(
                self._description, "setPrivate:", None, (ctypes.c_bool,), self._is_private
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
        frameworks = mac_frameworks()
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
        frameworks = mac_frameworks()
        uuid = frameworks.send_message(self._description, "UUID")
        uuid_string = frameworks.send_message(uuid, "UUIDString")
        return frameworks.send_message(uuid_string, "UTF8String", ctypes.c_char_p).decode(
            "ascii"
        )

    def is_private(self) -> bool:
        return bool(
            mac_frameworks().send_message(self._description, "isPrivate", ctypes.c_bool)
        )

    def mute_behaviour(self) -> str:
        value = mac_frameworks().send_message(self._description, "isMuted", ctypes.c_long)
        return {number: name for name, number in TAP_MUTE_BEHAVIOURS.items()}.get(
            value, str(value)
        )


class _PrivateProcessTapDescription(_TapDescription):
    """A private stereo mixdown of one process's output."""

    def __init__(self, process_object_id: int, mute_behaviour: str, name: str) -> None:
        super().__init__(
            "initStereoMixdownOfProcesses:", [process_object_id], True, mute_behaviour, name
        )


class _SharedGlobalTapDescription(_TapDescription):
    """A stereo mixdown of every process's output, visible to every process."""

    def __init__(self, mute_behaviour: str, name: str) -> None:
        super().__init__(
            "initStereoGlobalTapButExcludeProcesses:", [], False, mute_behaviour, name
        )


def _raise_on_failure(call_name: str, status: int) -> None:
    if status != 0:
        raise CoreAudioFixtureError(
            f"{call_name} failed with OSStatus {status} ('{four_char_code_text(status)}')"
        )


class _CaptureDeviceReadingATap:
    """A tap and the aggregate device it is read through, made on entry and destroyed on exit.

    Enter it before the graph opens a device, so a muted tap is muting from the
    first sample played rather than after an audible start.
    """

    _aggregate_device_is_private = True

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

    def _tap_description(self) -> _TapDescription:
        raise NotImplementedError

    def __enter__(self) -> "_CaptureDeviceReadingATap":
        core_audio = mac_frameworks().core_audio
        with self._tap_description() as tap_description:
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
                        self.aggregate_device_uid,
                        self.tap_uid,
                        self.clock_device_uid,
                        is_private=self._aggregate_device_is_private,
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
        core_audio = mac_frameworks().core_audio
        if self.aggregate_device_object_id != AUDIO_OBJECT_UNKNOWN:
            core_audio.AudioHardwareDestroyAggregateDevice(self.aggregate_device_object_id)
            self.aggregate_device_object_id = AUDIO_OBJECT_UNKNOWN
        if self.tap_object_id != AUDIO_OBJECT_UNKNOWN:
            core_audio.AudioHardwareDestroyProcessTap(self.tap_object_id)
            self.tap_object_id = AUDIO_OBJECT_UNKNOWN

    def tap_format(self) -> "tuple[float, int] | None":
        """The tap's sample rate and channel count, from its AudioStreamBasicDescription."""
        value = property_bytes(self.tap_object_id, TAP_PROPERTY_FORMAT)
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


class PrivateCaptureDeviceTappingThisProcessesOutput(_CaptureDeviceReadingATap):
    """This process's own output, readable as a capture device only it can see.

    A process that dies before leaving takes the tap and the aggregate with it,
    because both are private.
    """

    def _tap_description(self) -> _TapDescription:
        this_process_object = process_object_of(os.getpid())
        if this_process_object == AUDIO_OBJECT_UNKNOWN:
            raise CoreAudioFixtureError(
                f"the HAL has no process object for this process ({os.getpid()}), "
                "so there is nothing to tap"
            )
        return _PrivateProcessTapDescription(
            this_process_object, self.mute_behaviour, f"StreamLib fixture tap of {os.getpid()}"
        )


class SharedCaptureDeviceTappingAllOutput(_CaptureDeviceReadingATap):
    """Every process's output, readable as a capture device any process can open.

    It hears whatever else plays while it exists, and muted it keeps all of it
    off the hardware. Shared objects are not tied to this process, so leaving is
    what destroys them.
    """

    _aggregate_device_is_private = False

    def _tap_description(self) -> _TapDescription:
        return _SharedGlobalTapDescription(
            self.mute_behaviour, f"StreamLib fixture shared tap {self.aggregate_device_uid}"
        )


def hold_a_shared_tap_of_all_output(
    aggregate_device_uid: str, mute_behaviour: str, clock_device_uid: str
) -> int:
    """Make the shared tap, say `READY <evidence>` on stdout, and hold it until signalled."""

    def leave_on_signal(_signal_number, _frame) -> None:
        raise SystemExit(0)

    for stopping_signal in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(stopping_signal, leave_on_signal)
    try:
        with SharedCaptureDeviceTappingAllOutput(
            aggregate_device_uid, mute_behaviour, clock_device_uid
        ) as shared_capture_device:
            print(f"READY {shared_capture_device.evidence()}", flush=True)
            while True:
                signal.pause()
    except CoreAudioFixtureError as refusal:
        print(f"ERROR: {refusal}", file=sys.stderr)
        return 1


class RunningAudioDeviceIOProc:
    """An IOProc on one device, started on entry and stopped and destroyed on exit.

    `on_io_cycle(input_buffer_list, input_time_stamp, output_buffer_list)` is
    called with raw addresses on the HAL's IO thread. An exception there has
    nowhere to go, so the first is kept, every one is counted, and the cycle
    still reports success to the HAL.
    """

    def __init__(self, device_object_id: int, on_io_cycle) -> None:
        self.device_object_id = device_object_id
        self._on_io_cycle = on_io_cycle
        self._io_proc = AUDIO_DEVICE_IO_PROC(self._io_cycle)
        self._io_proc_id = ctypes.c_void_p()
        self.first_exception_on_the_io_thread: Optional[str] = None
        self.exceptions_on_the_io_thread = 0

    def _io_cycle(
        self, _device, _now, input_data, input_time, output_data, _output_time, _client
    ) -> int:
        try:
            self._on_io_cycle(input_data, input_time, output_data)
        except BaseException as exception:
            self.exceptions_on_the_io_thread += 1
            if self.first_exception_on_the_io_thread is None:
                self.first_exception_on_the_io_thread = repr(exception)
        return 0

    def __enter__(self) -> "RunningAudioDeviceIOProc":
        core_audio = mac_frameworks().core_audio
        _raise_on_failure(
            "AudioDeviceCreateIOProcID",
            core_audio.AudioDeviceCreateIOProcID(
                self.device_object_id, self._io_proc, None, ctypes.byref(self._io_proc_id)
            ),
        )
        try:
            _raise_on_failure(
                "AudioDeviceStart",
                core_audio.AudioDeviceStart(self.device_object_id, self._io_proc_id),
            )
        except BaseException:
            core_audio.AudioDeviceDestroyIOProcID(self.device_object_id, self._io_proc_id)
            self._io_proc_id = ctypes.c_void_p()
            raise
        return self

    def __exit__(self, *_exception) -> None:
        if not self._io_proc_id:
            return
        core_audio = mac_frameworks().core_audio
        # Called off the IO thread, AudioDeviceStop returns only once the
        # IOProc will not be called again, so destroying it next is safe.
        core_audio.AudioDeviceStop(self.device_object_id, self._io_proc_id)
        core_audio.AudioDeviceDestroyIOProcID(self.device_object_id, self._io_proc_id)
        self._io_proc_id = ctypes.c_void_p()


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
    if command == "authorize-a-tap":
        # TCC's answer on stdout, for the caller to judge the run by later.
        authorization = system_audio_recording_authorization()
        print(authorization)
        refusal = refusal_to_create_a_tap(authorization, this_run_is_attended())
        if refusal is not None:
            print(f"SKIP: {refusal}", file=sys.stderr)
            return 77
        return 0
    if (
        command == "hold-a-shared-tap-of-all-output"
        and len(argv) == 5
        and argv[3] in TAP_MUTE_BEHAVIOURS
    ):
        return hold_a_shared_tap_of_all_output(argv[2], argv[3], argv[4])
    if command == "explain-exact-zeros" and len(argv) == 3:
        status, reason = why_a_tap_delivered_exact_zeros(
            argv[2], system_audio_recording_authorization()
        )
        print(reason, file=sys.stderr)
        return status
    print(
        "Usage: coreaudio_process_tap.py devices | built-in-speaker-uid | "
        "built-in-microphone-uid | built-in-microphone-name | default-output-uid | "
        "system-audio-recording-authorization | authorize-a-tap | "
        "explain-exact-zeros <authorization-before-the-run> | "
        "hold-a-shared-tap-of-all-output <aggregate-device-uid> muted|unmuted <clock-device-uid>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
